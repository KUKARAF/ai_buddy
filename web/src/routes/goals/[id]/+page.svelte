<script lang="ts">
	import { onMount, onDestroy, tick } from 'svelte';
	import { page } from '$app/state';
	import { resolve } from '$app/paths';
	import { goto } from '$app/navigation';
	import Icon from '$lib/components/Icon.svelte';
	import Markdown from '$lib/components/Markdown.svelte';
	import { app } from '$lib/appState.svelte';
	import { shortDate, relativeDate, euros } from '$lib/format';
	import { goalLook } from '$lib/goalView';
	import { buildIcs, downloadIcs, icsFilename, type IcsEvent } from '$lib/ics';
	import {
		getGoal,
		getRoadmap,
		listGoalCheckIns,
		getPledge,
		patchStep,
		patchGoal,
		createGoalCheckIn,
		uploadCheckInPhoto,
		fetchCheckInPhoto,
		adjustPlan,
		createPledge,
		confirmPledge,
		getWallet,
		getSimilar,
		ApiError,
		type Goal,
		type Roadmap,
		type RoadmapStep,
		type Break,
		type CheckIn,
		type Pledge,
		type SimilarGoals,
		type AdjustProposal
	} from '$lib/api/client';

	const goalId = page.params.id ?? '';

	let goal = $state<Goal | null>(null);
	let roadmap = $state<Roadmap | null>(null);
	let checkIns = $state<CheckIn[]>([]);
	let pledge = $state<Pledge | null>(null);
	let walletCents = $state<number | null>(null);
	let similar = $state<SimilarGoals | null>(null);

	let loading = $state(true);
	let loadState = $state<'ok' | 'guest' | 'notfound' | 'error'>('ok');

	type Tab = 'plan' | 'checkins' | 'resources';
	let tab = $state<Tab>('plan');

	// step toggle guard
	const toggling: Record<string, true> = {};

	// check-in composer
	let composerOpen = $state(false);
	let note = $state('');
	let checkinSubmitting = $state(false);

	// Photo attachment for the composer + per-check-in thumbnail object URLs.
	let photoInput = $state<HTMLInputElement | null>(null);
	let photoFile = $state<File | null>(null);
	let photoError = $state<string | null>(null);
	let photoUrls = $state<Record<string, string>>({});
	const photoLoading: Record<string, true> = {};
	// Full-size photo viewer (object URL shared with the thumbnail — don't revoke here).
	let lightbox = $state<string | null>(null);

	// "Adjust my plan" mini-chat (agentic, non-streaming coach turn).
	interface AdjustMessage {
		id: string;
		role: 'user' | 'assistant';
		content: string;
		/** Structured preview of the coach's proposed change (assistant turns only). */
		proposal?: AdjustProposal | null;
	}
	let adjustOpen = $state(false);
	let adjustMessages = $state<AdjustMessage[]>([]);
	let adjustDraft = $state('');
	let adjustSending = $state(false);
	let adjustError = $state<string | null>(null);
	let adjustNeedsTopup = $state(false);
	let adjustUnavailable = $state(false);
	let adjustChanged = $state(false);
	let adjustThread = $state<HTMLElement | null>(null);

	// pledge form
	let pledgeAmount = $state('4');
	let pledgeSubmitting = $state(false);
	let pledgeError = $state<{ wallet: boolean; message: string } | null>(null);

	onMount(async () => {
		try {
			goal = await getGoal(goalId);
		} catch (err) {
			if (err instanceof ApiError && err.status === 401) loadState = 'guest';
			else if (err instanceof ApiError && err.status === 404) loadState = 'notfound';
			else loadState = 'error';
			loading = false;
			return;
		}
		loading = false;
		// Best-effort side data.
		void getRoadmap(goalId)
			.then((r) => (roadmap = r))
			.catch(() => (roadmap = null));
		void listGoalCheckIns(goalId)
			.then((c) => (checkIns = c))
			.catch(() => (checkIns = []));
		void getPledge(goalId)
			.then((p) => (pledge = p))
			.catch(() => (pledge = null));
		void getWallet()
			.then((w) => (walletCents = w.balance_cents))
			.catch(() => (walletCents = null));
		void getSimilar(goalId)
			.then((s) => (similar = s))
			.catch(() => (similar = null));
	});

	// Real count of other users with a similar goal (0 = honest normal case).
	const similarCount = $derived(similar?.similar_user_count ?? 0);

	const look = $derived(goal ? goalLook(goal) : null);
	const steps = $derived<RoadmapStep[]>(roadmap?.steps ?? []);
	const doneCount = $derived(steps.filter((s) => s.status === 'done').length);
	const pct = $derived(steps.length > 0 ? Math.round((doneCount / steps.length) * 100) : 0);
	const allDone = $derived(steps.length > 0 && doneCount === steps.length);

	// Milestones + planned breaks, merged into one chronological timeline. When
	// there are no breaks this is exactly the milestone list (same look as before).
	// Milestone numbers stay tied to the milestone's own order, not merged index.
	type TimelineItem =
		| { kind: 'step'; step: RoadmapStep; num: number; date: string | null }
		| { kind: 'break'; brk: Break; date: string };

	const timeline = $derived.by<TimelineItem[]>(() => {
		const items: TimelineItem[] = steps.map((step, i) => ({
			kind: 'step',
			step,
			num: i + 1,
			date: step.due_date
		}));
		const breaks = [...(roadmap?.breaks ?? [])].sort((a, b) =>
			a.start_date < b.start_date ? -1 : a.start_date > b.start_date ? 1 : 0
		);
		if (breaks.length === 0) return items;
		const merged: TimelineItem[] = [...items];
		for (const brk of breaks) {
			// Slot the break before the first later-dated milestone; otherwise last.
			let idx = merged.findIndex((m) => m.date !== null && m.date > brk.start_date);
			if (idx === -1) idx = merged.length;
			merged.splice(idx, 0, { kind: 'break', brk, date: brk.start_date });
		}
		return merged;
	});

	const metaBits = $derived.by(() => {
		if (!goal) return [];
		const bits: string[] = [];
		if (goal.location) bits.push(goal.location);
		const d = shortDate(goal.deadline);
		if (d) bits.push(`${d} finish line`);
		bits.push('Private');
		return bits;
	});

	async function toggleStep(step: RoadmapStep): Promise<void> {
		if (toggling[step.id] || !roadmap) return;
		toggling[step.id] = true;
		const nextStatus = step.status === 'done' ? 'pending' : 'done';
		try {
			const updated = await patchStep(step.id, nextStatus);
			roadmap = {
				...roadmap,
				steps: roadmap.steps.map((s) => (s.id === step.id ? { ...s, status: updated.status } : s))
			};
		} catch {
			/* leave as-is on failure */
		} finally {
			delete toggling[step.id];
		}
	}

	async function submitCheckIn(e: SubmitEvent): Promise<void> {
		e.preventDefault();
		const text = note.trim();
		if (text === '' || checkinSubmitting) return;
		checkinSubmitting = true;
		photoError = null;
		let created;
		try {
			created = await createGoalCheckIn(goalId, { note: text });
			checkIns = [created, ...checkIns];
			note = '';
			void app.refreshStreak();
		} catch {
			// Keep the text (and the chosen photo) so the user can retry.
			checkinSubmitting = false;
			return;
		}
		// Photo is best-effort: the check-in already succeeded above.
		const file = photoFile;
		photoFile = null;
		if (file) await attachPhoto(created.id, file);
		composerOpen = false;
		checkinSubmitting = false;
		// When the coach wants to adjust, don't show a passive card: open the
		// mini-chat and immediately send the note so it PROPOSES concrete changes.
		if (created.suggestion?.adjust) {
			void openAdjustFromCheckIn(text);
		}
	}

	// --- Photos ----------------------------------------------------------------

	function onPhotoPick(e: Event): void {
		const input = e.currentTarget as HTMLInputElement;
		photoFile = input.files?.[0] ?? null;
		photoError = null;
		// Clear so re-picking the same file still fires change.
		input.value = '';
	}

	function canvasToBlob(canvas: HTMLCanvasElement, quality: number): Promise<Blob> {
		return new Promise((resolve, reject) => {
			canvas.toBlob(
				(b) => (b ? resolve(b) : reject(new Error('encode failed'))),
				'image/jpeg',
				quality
			);
		});
	}

	// Downscale client-side (max ~1600px, JPEG ~0.8) so the upload fits the
	// backend's 1 MiB body limit; step quality down if it's still too big.
	async function downscaleImage(file: File): Promise<Blob> {
		const MAX_DIM = 1600;
		const TARGET_BYTES = 900 * 1024;
		const dataUrl = await new Promise<string>((resolve, reject) => {
			const reader = new FileReader();
			reader.onload = () => resolve(reader.result as string);
			reader.onerror = () => reject(new Error('read failed'));
			reader.readAsDataURL(file);
		});
		const img = await new Promise<HTMLImageElement>((resolve, reject) => {
			const el = new Image();
			el.onload = () => resolve(el);
			el.onerror = () => reject(new Error('decode failed'));
			el.src = dataUrl;
		});
		let width = img.naturalWidth || img.width;
		let height = img.naturalHeight || img.height;
		if (width > MAX_DIM || height > MAX_DIM) {
			const scale = MAX_DIM / Math.max(width, height);
			width = Math.round(width * scale);
			height = Math.round(height * scale);
		}
		const canvas = document.createElement('canvas');
		canvas.width = width;
		canvas.height = height;
		const ctx = canvas.getContext('2d');
		if (!ctx) throw new Error('no canvas context');
		ctx.drawImage(img, 0, 0, width, height);
		let quality = 0.8;
		let blob = await canvasToBlob(canvas, quality);
		while (blob.size > TARGET_BYTES && quality > 0.4) {
			quality -= 0.15;
			blob = await canvasToBlob(canvas, quality);
		}
		return blob;
	}

	async function attachPhoto(checkInId: string, file: File): Promise<void> {
		try {
			const blob = await downscaleImage(file);
			await uploadCheckInPhoto(checkInId, blob);
			checkIns = checkIns.map((c) => (c.id === checkInId ? { ...c, has_photo: true } : c));
			void loadPhoto(checkInId);
		} catch {
			// The check-in itself saved — only the photo failed.
			photoError = 'Your check-in saved, but the photo could not be uploaded.';
		}
	}

	// Fetch a check-in's photo (authenticated) and keep it as an object URL. Never
	// shows a broken image: on any failure we simply don't add a URL.
	async function loadPhoto(id: string): Promise<void> {
		if (photoUrls[id] || photoLoading[id]) return;
		photoLoading[id] = true;
		try {
			const blob = await fetchCheckInPhoto(id);
			if (blob) photoUrls = { ...photoUrls, [id]: URL.createObjectURL(blob) };
		} catch {
			/* skip — no thumbnail rather than a broken image */
		} finally {
			delete photoLoading[id];
		}
	}

	// Load thumbnails for any check-in that has a photo as the list arrives/updates.
	$effect(() => {
		for (const ci of checkIns) {
			if (ci.has_photo) void loadPhoto(ci.id);
		}
	});

	onDestroy(() => {
		for (const url of Object.values(photoUrls)) URL.revokeObjectURL(url);
	});

	// --- Add to calendar (.ics, pure client-side) ------------------------------

	const hasCalendarItems = $derived(
		(goal?.deadline ?? null) !== null || steps.some((s) => s.due_date)
	);

	function addStepToCalendar(step: RoadmapStep): void {
		if (!step.due_date || !goal) return;
		const ics = buildIcs([
			{ uid: step.id, date: step.due_date, summary: step.title, description: goal.title }
		]);
		downloadIcs(icsFilename(step.title), ics);
	}

	function addFinishLineToCalendar(): void {
		if (!goal?.deadline) return;
		const ics = buildIcs([
			{
				uid: goal.id,
				date: goal.deadline,
				summary: `Finish line: ${goal.title}`,
				description: goal.success_criterion ?? goal.title
			}
		]);
		downloadIcs(icsFilename(`${goal.title}-finish-line`), ics);
	}

	function addWholePlanToCalendar(): void {
		if (!goal) return;
		const events: IcsEvent[] = steps
			.filter((s) => s.due_date)
			.map((s) => ({
				uid: s.id,
				date: s.due_date as string,
				summary: s.title,
				description: goal!.title
			}));
		if (goal.deadline) {
			events.push({
				uid: goal.id,
				date: goal.deadline,
				summary: `Finish line: ${goal.title}`,
				description: goal.success_criterion ?? goal.title
			});
		}
		if (events.length === 0) return;
		downloadIcs(icsFilename(goal.title), buildIcs(events));
	}

	async function scrollAdjust(): Promise<void> {
		await tick();
		if (adjustThread) adjustThread.scrollTop = adjustThread.scrollHeight;
	}

	async function sendAdjust(content: string): Promise<void> {
		if (content === '' || adjustSending) return;
		adjustError = null;
		adjustNeedsTopup = false;
		adjustUnavailable = false;
		adjustSending = true;
		adjustMessages = [...adjustMessages, { id: `u-${Date.now()}`, role: 'user', content }];
		void scrollAdjust();
		try {
			const res = await adjustPlan(goalId, content);
			adjustMessages = [
				...adjustMessages,
				{ id: `a-${Date.now()}`, role: 'assistant', content: res.reply, proposal: res.proposal }
			];
			// The coach changed the plan/breaks — refresh so new milestones/dates/breaks render now.
			if (res.changed) {
				adjustChanged = true;
				void getRoadmap(goalId)
					.then((r) => (roadmap = r))
					.catch(() => {});
				void listGoalCheckIns(goalId)
					.then((c) => (checkIns = c))
					.catch(() => {});
			}
		} catch (err) {
			if (err instanceof ApiError && err.status === 402) {
				adjustNeedsTopup = true;
			} else if (err instanceof ApiError && err.status === 400) {
				adjustUnavailable = true;
			} else {
				adjustError = err instanceof ApiError ? err.message : 'Failed to reach your coach.';
			}
		} finally {
			adjustSending = false;
			void scrollAdjust();
		}
	}

	function openAdjust(): void {
		adjustOpen = true;
		adjustError = null;
		adjustNeedsTopup = false;
		adjustUnavailable = false;
	}

	// Entered from a check-in whose coach suggestion asked to adjust: open the
	// panel on a fresh thread and immediately send the note so the coach proposes
	// concrete changes the user can confirm with a reply (e.g. "yes").
	async function openAdjustFromCheckIn(seed: string): Promise<void> {
		adjustOpen = true;
		adjustError = null;
		adjustNeedsTopup = false;
		adjustUnavailable = false;
		adjustChanged = false;
		adjustMessages = [];
		await sendAdjust(seed.trim());
	}

	function submitAdjust(e: SubmitEvent): void {
		e.preventDefault();
		const content = adjustDraft.trim();
		if (content === '' || adjustSending) return;
		adjustDraft = '';
		void sendAdjust(content);
	}

	function closeAdjust(): void {
		adjustOpen = false;
	}

	type MilestoneChange = AdjustProposal['milestone_changes'][number];

	/** Drop no-op milestone changes (same date before and after) from the card. */
	function visibleMilestoneChanges(changes: MilestoneChange[]): MilestoneChange[] {
		return changes.filter((c) => !(c.old_due !== null && c.old_due === c.new_due));
	}

	/** How to render one milestone change: a new/removed badge or a before→after move. */
	function milestoneChangeKind(c: MilestoneChange): 'new' | 'removed' | 'moved' {
		if (c.old_due === null) return 'new';
		if (c.new_due === null) return 'removed';
		return 'moved';
	}

	/** True when the proposal has anything worth drawing a card for. */
	function hasProposalContent(p: AdjustProposal): boolean {
		return (
			p.summary.trim() !== '' ||
			p.add_breaks.length > 0 ||
			p.remove_breaks.length > 0 ||
			visibleMilestoneChanges(p.milestone_changes).length > 0
		);
	}

	async function submitPledge(e: SubmitEvent): Promise<void> {
		e.preventDefault();
		pledgeError = null;
		const cents = Math.round(parseFloat(pledgeAmount || '0') * 100);
		if (!Number.isFinite(cents) || cents < 400) {
			pledgeError = { wallet: false, message: 'Minimum pledge is €4.' };
			return;
		}
		pledgeSubmitting = true;
		try {
			await createPledge(goalId, cents);
			pledge = await confirmPledge(goalId);
		} catch (err) {
			if (err instanceof ApiError && err.status === 402) {
				pledgeError = { wallet: true, message: 'Your wallet balance is too low for this pledge.' };
			} else {
				pledgeError = {
					wallet: false,
					message: err instanceof ApiError ? err.message : 'Could not place your pledge.'
				};
			}
		} finally {
			pledgeSubmitting = false;
		}
	}

	async function markComplete(): Promise<void> {
		if (!goal) return;
		try {
			goal = await patchGoal(goalId, { status: 'succeeded' });
			void app.refreshGoals();
			void getPledge(goalId)
				.then((p) => (pledge = p))
				.catch(() => {});
		} catch {
			/* ignore */
		}
	}

	async function archiveGoal(): Promise<void> {
		if (!goal) return;
		if (!confirm('Archive this goal? It will move out of your active goals.')) return;
		try {
			await patchGoal(goalId, { status: 'abandoned' });
			void app.refreshGoals();
			void goto(resolve('/goals'));
		} catch {
			/* ignore */
		}
	}

	function openCheckin(): void {
		tab = 'checkins';
		composerOpen = true;
	}

	const pledgeStatusText: Record<string, string> = {
		proposed: 'Proposed',
		held: 'Held — refunded when you follow through',
		refunded: 'Refunded — you did it!',
		forfeited: 'Forfeited'
	};
</script>

<div class="detail">
	<a class="crumb" href={resolve('/goals')}><Icon name="chevron-l" size={15} /> Back to my goals</a>

	{#if loading}
		<p class="muted state">Loading your plan…</p>
	{:else if loadState === 'guest'}
		<div class="card state-card">
			<span class="tile tile-sky"><Icon name="lock" size={24} /></span>
			<h2>This goal is private</h2>
			<p class="muted">Sign in to view your plan.</p>
		</div>
	{:else if loadState === 'notfound'}
		<div class="card state-card">
			<span class="tile tile-lavender"><Icon name="compass" size={24} /></span>
			<h2>Goal not found</h2>
			<p class="muted">It may have been archived.</p>
			<a class="btn" href={resolve('/goals')}>Back to my goals</a>
		</div>
	{:else if loadState === 'error'}
		<p class="error state">Something went wrong loading this goal.</p>
	{:else if goal && look}
		<!-- HEADER -->
		<header class="g-head">
			<div class="g-head-main">
				<span class="pill">{look.category}</span>
				<h1>{goal.title}</h1>
				<p class="meta muted">{metaBits.join(' · ')}</p>
			</div>
			<button class="btn checkin-btn" onclick={openCheckin}>
				<Icon name="check" size={16} stroke={2.4} /> Check in
			</button>
		</header>

		{#if goal.status === 'succeeded'}
			<div class="won">
				<Icon name="check" size={16} stroke={2.6} /> You did it — this goal is complete.
			</div>
		{/if}

		{#if goal.success_criterion}
			<section class="card finish">
				<p class="eyebrow">Your finish line</p>
				<p class="finish-text">{goal.success_criterion}</p>
			</section>
		{/if}

		<!-- TABS -->
		<div class="tabs" role="tablist">
			<button
				class="tab"
				class:on={tab === 'plan'}
				role="tab"
				aria-selected={tab === 'plan'}
				onclick={() => (tab = 'plan')}>The plan</button
			>
			<button
				class="tab"
				class:on={tab === 'checkins'}
				role="tab"
				aria-selected={tab === 'checkins'}
				onclick={() => (tab = 'checkins')}>Check-ins</button
			>
			<button
				class="tab"
				class:on={tab === 'resources'}
				role="tab"
				aria-selected={tab === 'resources'}
				onclick={() => (tab = 'resources')}>Resources</button
			>
		</div>

		<div class="layout">
			<div class="col">
				{#if tab === 'plan'}
					<section>
						<div class="plan-head">
							<div>
								<h2>Small steps. Real progress.</h2>
								<p class="muted sub">
									A starter framework you control — tap a step to mark it done.
								</p>
							</div>
							{#if steps.length > 0}
								<button class="btn btn-ghost adjust-open" onclick={() => openAdjust()}>
									<Icon name="sparkle" size={15} /> Adjust my plan
								</button>
							{/if}
						</div>

						{#if steps.length === 0}
							<div class="card empty-plan">
								<p class="muted">Your plan isn't ready yet.</p>
								<p class="muted small">We'll build your milestones when coaching is available.</p>
							</div>
						{:else}
							{#if hasCalendarItems}
								<div class="cal-bar">
									<button
										type="button"
										class="btn btn-ghost cal-all"
										onclick={addWholePlanToCalendar}
									>
										📅 Add whole plan to calendar
									</button>
									{#if goal.deadline}
										<button
											type="button"
											class="btn btn-ghost cal-all"
											onclick={addFinishLineToCalendar}
										>
											🏁 Add finish line
										</button>
									{/if}
								</div>
							{/if}

							<ol class="timeline">
								{#each timeline as item (item.kind === 'step' ? item.step.id : `brk-${item.brk.id}`)}
									{#if item.kind === 'step'}
										<li class:done={item.step.status === 'done'}>
											<button
												class="node"
												aria-label={item.step.status === 'done'
													? 'Mark step not done'
													: 'Mark step done'}
												onclick={() => toggleStep(item.step)}
											>
												{#if item.step.status === 'done'}
													<Icon name="check" size={15} stroke={2.6} />
												{:else}
													{String(item.num).padStart(2, '0')}
												{/if}
											</button>
											<div class="ms-body">
												<p class="eyebrow">Milestone {item.num}</p>
												<h3>{item.step.title}</h3>
												{#if item.step.detail}<p class="muted">{item.step.detail}</p>{/if}
												{#if shortDate(item.step.due_date)}
													<p class="ms-due muted small">🗓 by {shortDate(item.step.due_date)}</p>
												{/if}
												{#if item.step.status === 'done'}
													<span class="chip-status completed">Completed</span>
												{:else if item.step.effort}
													<span class="chip-status">{item.step.effort}</span>
												{/if}
												{#if item.step.due_date}
													<button
														type="button"
														class="cal-btn"
														onclick={() => addStepToCalendar(item.step)}
													>
														📅 Add to calendar
													</button>
												{/if}
											</div>
										</li>
									{:else}
										<li class="brk">
											<span class="brk-node" aria-hidden="true">🌴</span>
											<div class="brk-card">
												<p class="brk-text">
													🌴 {item.brk.label} · {shortDate(item.brk.start_date)}–{shortDate(
														item.brk.end_date
													)}
												</p>
												<span class="brk-tag">planned break</span>
											</div>
										</li>
									{/if}
								{/each}
							</ol>

							<div class="card progress-card">
								<p class="eyebrow">Look how far you've come</p>
								<p class="prog-num"><strong>{pct}%</strong> of your milestones</p>
								<div class="progress"><span style="width:{pct}%"></span></div>
								<p class="muted small encourage">
									{#if pct === 0}
										Every plan starts with one small step. Yours is right above.
									{:else if pct < 100}
										You're moving. Keep coming back — that's the whole trick.
									{:else}
										Every milestone done. Time to call it.
									{/if}
								</p>
								{#if allDone && goal.status !== 'succeeded'}
									<button class="btn btn-lime complete" onclick={markComplete}>
										<Icon name="check" size={16} stroke={2.4} /> Mark complete
									</button>
								{/if}
							</div>
						{/if}
					</section>
				{:else if tab === 'checkins'}
					<section>
						<div class="ci-head">
							<h2>Check-ins</h2>
							<button class="btn btn-ghost" onclick={() => (composerOpen = !composerOpen)}>
								<Icon name="plus" size={15} /> New check-in
							</button>
						</div>

						{#if composerOpen}
							<form class="card composer" onsubmit={submitCheckIn}>
								<textarea
									rows="3"
									bind:value={note}
									placeholder="How did it go? Even a messy day counts."></textarea>
								<input
									type="file"
									accept="image/*"
									bind:this={photoInput}
									onchange={onPhotoPick}
									hidden
								/>
								<div class="composer-foot">
									<div class="attach">
										<button
											type="button"
											class="btn btn-ghost attach-btn"
											onclick={() => photoInput?.click()}
										>
											📷 Add photo
										</button>
										{#if photoFile}
											<span class="attach-name">
												{photoFile.name}
												<button
													type="button"
													class="attach-x"
													aria-label="Remove photo"
													onclick={() => (photoFile = null)}>×</button
												>
											</span>
										{/if}
									</div>
									<button
										type="submit"
										class="btn"
										disabled={checkinSubmitting || note.trim() === ''}
									>
										{checkinSubmitting ? 'Saving…' : 'Save check-in'}
									</button>
								</div>
								{#if photoError}<p class="error small photo-err">{photoError}</p>{/if}
							</form>
						{/if}

						{#if checkIns.length === 0}
							<div class="card empty-plan">
								<p class="muted">No check-ins yet.</p>
								<p class="muted small">Log the first one — showing up is the win.</p>
							</div>
						{:else}
							<ul class="ci-list">
								{#each checkIns as ci (ci.id)}
									<li class="card ci-item">
										<span class="ci-dot"><Icon name="check" size={13} stroke={2.6} /></span>
										<div class="ci-body">
											<p class="ci-note">{ci.note ?? 'Checked in.'}</p>
											{#if ci.has_photo && photoUrls[ci.id]}
												<button
													type="button"
													class="ci-photo"
													aria-label="View check-in photo"
													onclick={() => (lightbox = photoUrls[ci.id])}
												>
													<img src={photoUrls[ci.id]} alt="Check-in snapshot" />
												</button>
											{/if}
											<span class="muted small">{relativeDate(ci.created_at)}</span>
										</div>
									</li>
								{/each}
							</ul>
						{/if}
					</section>
				{:else}
					<section>
						<h2>Resources</h2>
						<div class="card empty-plan">
							<span class="tile tile-lavender"><Icon name="sparkle" size={22} /></span>
							<p class="muted">Handpicked tips and tools for this goal are coming soon.</p>
						</div>
					</section>
				{/if}
			</div>

			<!-- SIDE: pledge + actions -->
			<aside class="side">
				{#if similarCount > 0}
					<section class="card community">
						<span class="community-ic"><Icon name="people" size={18} /></span>
						<p>
							<strong>{similarCount}</strong> other {similarCount === 1 ? 'person' : 'people'} working
							on something similar.
						</p>
					</section>
				{/if}

				<section class="card pledge">
					<p class="eyebrow">A promise to yourself</p>
					{#if pledge}
						<p class="pledge-amount"><strong>{euros(pledge.amount_cents)}</strong></p>
						<span class="chip-status" class:completed={pledge.status === 'refunded'}>
							{pledgeStatusText[pledge.status] ?? pledge.status}
						</span>
						<p class="muted small pledge-note">
							Held from your wallet, refunded when you follow through.
						</p>
					{:else}
						<p class="muted small">
							Put a little belief behind it. A pledge is held from your wallet and refunded when you
							succeed.
						</p>
						<form class="pledge-form" onsubmit={submitPledge}>
							<div class="inputrow">
								<span class="cur">€</span>
								<input
									type="number"
									min="4"
									step="1"
									bind:value={pledgeAmount}
									disabled={pledgeSubmitting}
								/>
							</div>
							<button class="btn btn-lime" type="submit" disabled={pledgeSubmitting}>
								{pledgeSubmitting ? 'Placing…' : 'Put a pledge behind it'}
							</button>
						</form>
						{#if walletCents !== null}
							<p class="muted small">Wallet balance: {euros(walletCents)}</p>
						{/if}
						{#if pledgeError}
							<p class="error small">{pledgeError.message}</p>
							{#if pledgeError.wallet}
								<a class="wallet-link" href={resolve('/wallet')}
									>Top up your wallet <Icon name="chevron-r" size={13} /></a
								>
							{/if}
						{/if}
					{/if}
				</section>

				<section class="card actions-card">
					{#if !allDone && goal.status !== 'succeeded'}
						<button class="act" onclick={markComplete}
							><Icon name="check" size={16} /> Mark complete</button
						>
					{/if}
					<button class="act danger" onclick={archiveGoal}
						><Icon name="x" size={16} /> Archive goal</button
					>
				</section>
			</aside>
		</div>

		{#if adjustOpen}
			<div
				class="adjust-backdrop"
				role="button"
				tabindex="-1"
				aria-label="Close"
				onclick={closeAdjust}
				onkeydown={(e) => e.key === 'Escape' && closeAdjust()}
			></div>
			<div class="adjust-modal card" role="dialog" aria-modal="true" aria-label="Adjust my plan">
				<header class="adjust-head">
					<span class="adjust-badge"><Icon name="sparkle" size={16} /></span>
					<div class="adjust-title">
						<h2>Adjust my plan</h2>
						<p class="muted small">Tell your coach what changed — it'll reshape your timeline.</p>
					</div>
					<button class="adjust-close" aria-label="Close" onclick={closeAdjust}>
						<Icon name="x" size={18} />
					</button>
				</header>

				<section class="adjust-thread" aria-live="polite" bind:this={adjustThread}>
					{#if adjustMessages.length === 0}
						<div class="row assistant">
							<span class="av"><Icon name="check" size={14} stroke={2.6} /></span>
							<div class="bubble a-bubble">
								What would you like to change? For example: "I'll be away over Christmas — add the
								break and shift everything after it."
							</div>
						</div>
					{/if}
					{#each adjustMessages as message (message.id)}
						{#if message.role === 'assistant'}
							<div class="row assistant">
								<span class="av"><Icon name="check" size={14} stroke={2.6} /></span>
								<div class="a-col">
									<div class="bubble a-bubble"><Markdown source={message.content} /></div>
									{#if message.proposal && hasProposalContent(message.proposal)}
										{@const prop = message.proposal}
										<div class="proposal">
											{#if prop.summary.trim() !== ''}
												<p class="prop-summary">{prop.summary}</p>
											{/if}
											{#if prop.add_breaks.length > 0}
												<div class="prop-group">
													<span class="prop-label">Breaks to add</span>
													<ul class="prop-list">
														{#each prop.add_breaks as brk, i (i)}
															<li>
																🌴 <strong>{brk.label}</strong>
																<span class="prop-dates"
																	>{shortDate(brk.start_date)}–{shortDate(brk.end_date)}</span
																>
															</li>
														{/each}
													</ul>
												</div>
											{/if}
											{#if prop.remove_breaks.length > 0}
												<div class="prop-group">
													<span class="prop-label">Breaks to remove</span>
													<ul class="prop-list">
														{#each prop.remove_breaks as label, i (i)}
															<li class="prop-remove">✕ {label}</li>
														{/each}
													</ul>
												</div>
											{/if}
											{#if visibleMilestoneChanges(prop.milestone_changes).length > 0}
												<div class="prop-group">
													<span class="prop-label">Milestone dates</span>
													<ul class="prop-list">
														{#each visibleMilestoneChanges(prop.milestone_changes) as c, i (i)}
															<li>
																<strong>{c.title}</strong>
																{#if milestoneChangeKind(c) === 'moved'}
																	<span class="prop-dates"
																		>{shortDate(c.old_due)} → {shortDate(c.new_due)}</span
																	>
																{:else if milestoneChangeKind(c) === 'new'}
																	<span class="prop-badge">new</span>
																	<span class="prop-dates">{shortDate(c.new_due)}</span>
																{:else}
																	<span class="prop-badge prop-badge-remove">removed</span>
																{/if}
															</li>
														{/each}
													</ul>
												</div>
											{/if}
										</div>
									{/if}
								</div>
							</div>
						{:else}
							<div class="row user">
								<div class="bubble u-bubble">{message.content}</div>
							</div>
						{/if}
					{/each}

					{#if adjustSending && adjustMessages[adjustMessages.length - 1]?.role === 'user'}
						<div class="row assistant">
							<span class="av"><Icon name="check" size={14} stroke={2.6} /></span>
							<div class="bubble a-bubble muted">Your coach is thinking…</div>
						</div>
					{/if}
				</section>

				{#if adjustChanged}
					<div class="notice notice-done">
						<Icon name="check" size={18} stroke={2.4} />
						<span>Your plan's updated — the new dates and breaks are in.</span>
					</div>
				{:else if adjustMessages.length > 0 && adjustMessages[adjustMessages.length - 1]?.role === 'assistant' && !adjustSending}
					<p class="confirm-hint muted small">
						Reply <strong>"yes"</strong> to apply these changes — or tell your coach what to tweak.
					</p>
				{/if}

				{#if adjustNeedsTopup}
					<div class="notice">
						<Icon name="wallet" size={18} />
						<span>Top up your wallet so the coach can adjust your plan.</span>
						<a class="btn btn-lime" href={resolve('/wallet')}>Open wallet</a>
					</div>
				{/if}
				{#if adjustUnavailable}
					<div class="notice">
						<Icon name="sparkle" size={18} />
						<span>Your coach isn't available right now. Please try again a little later.</span>
					</div>
				{/if}
				{#if adjustError}<p class="error small">{adjustError}</p>{/if}

				<form class="adjust-form" onsubmit={submitAdjust}>
					<input
						type="text"
						placeholder="Message your coach…"
						bind:value={adjustDraft}
						disabled={adjustSending}
					/>
					<button
						type="submit"
						class="sendbtn"
						disabled={adjustSending || adjustDraft.trim() === ''}
						aria-label="Send"
					>
						<Icon name="send" size={18} />
					</button>
				</form>
			</div>
		{/if}

		{#if lightbox}
			<div
				class="lightbox"
				role="button"
				tabindex="-1"
				aria-label="Close photo"
				onclick={() => (lightbox = null)}
				onkeydown={(e) => e.key === 'Escape' && (lightbox = null)}
			>
				<img src={lightbox} alt="Check-in snapshot" />
			</div>
		{/if}
	{/if}
</div>

<style>
	.detail {
		display: flex;
		flex-direction: column;
		gap: 20px;
		max-width: 1000px;
	}
	.crumb {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		color: var(--muted);
		font-weight: 600;
		font-size: 13px;
	}
	.state {
		padding: 40px 0;
	}
	.state-card {
		display: flex;
		flex-direction: column;
		align-items: center;
		text-align: center;
		gap: 12px;
		padding: 44px 24px;
		max-width: 440px;
		margin: 10px auto;
	}
	.state-card h2 {
		font-size: 20px;
	}

	.g-head {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 16px;
	}
	.g-head h1 {
		font-size: 30px;
		margin: 8px 0 6px;
		line-height: 1.15;
	}
	.meta {
		font-size: 14px;
		margin: 0;
	}
	.checkin-btn {
		flex: none;
	}
	.won {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		background: color-mix(in srgb, var(--lime) 30%, var(--card));
		border: 1px solid var(--line);
		border-radius: 12px;
		padding: 10px 14px;
		font-weight: 600;
		font-size: 14px;
	}
	.finish {
		padding: 16px 18px;
		border-left: 4px solid var(--lime);
	}
	.finish-text {
		margin: 4px 0 0;
		font-size: 16px;
		font-weight: 500;
	}

	.tabs {
		display: flex;
		gap: 6px;
		border-bottom: 1px solid var(--line);
	}
	.tab {
		background: transparent;
		border: none;
		border-bottom: 2px solid transparent;
		padding: 10px 6px;
		margin-bottom: -1px;
		font-size: 14px;
		font-weight: 600;
		color: var(--muted);
	}
	.tab.on {
		color: var(--ink);
		border-bottom-color: var(--lime);
	}

	.layout {
		display: flex;
		gap: 24px;
		align-items: flex-start;
	}
	.col {
		flex: 1;
		min-width: 0;
	}
	.col h2 {
		font-size: 20px;
	}
	.sub {
		font-size: 14px;
		margin: 4px 0 18px;
	}
	.side {
		width: 300px;
		flex: none;
		display: flex;
		flex-direction: column;
		gap: 16px;
	}

	.timeline {
		list-style: none;
		margin: 0;
		padding: 0;
		position: relative;
	}
	.timeline::before {
		content: '';
		position: absolute;
		left: 17px;
		top: 8px;
		bottom: 8px;
		width: 2px;
		background: var(--line);
	}
	.timeline li {
		position: relative;
		display: flex;
		gap: 16px;
		padding-bottom: 22px;
	}
	.node {
		width: 36px;
		height: 36px;
		border-radius: 999px;
		border: 2px solid var(--line);
		background: var(--card);
		color: var(--muted);
		font-size: 12px;
		font-weight: 700;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		flex: none;
		z-index: 1;
	}
	.timeline li.done .node {
		background: var(--lime);
		border-color: var(--lime);
		color: var(--ink);
	}
	.ms-body {
		padding-top: 2px;
	}
	.ms-body h3 {
		font-size: 16px;
		margin: 2px 0 4px;
	}
	.ms-body p {
		margin: 0 0 8px;
		font-size: 14px;
	}
	.ms-due {
		margin: 0 0 8px;
	}
	.chip-status {
		display: inline-block;
		background: var(--tag-bg);
		color: var(--muted);
		border-radius: 999px;
		padding: 3px 10px;
		font-size: 12px;
		font-weight: 600;
	}
	.chip-status.completed {
		background: color-mix(in srgb, var(--lime) 35%, var(--card));
		color: var(--ink);
	}
	.progress-card {
		padding: 18px;
		margin-top: 4px;
	}
	.prog-num {
		font-size: 15px;
		margin: 4px 0 10px;
	}
	.encourage {
		margin: 10px 0 0;
	}
	.complete {
		margin-top: 14px;
	}

	.ci-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		margin-bottom: 14px;
	}
	.composer {
		padding: 14px;
		margin-bottom: 14px;
	}
	.composer textarea {
		width: 100%;
		border: 1px solid var(--line);
		border-radius: 10px;
		padding: 10px;
		font-family: inherit;
		font-size: 14px;
		color: var(--ink);
		resize: vertical;
	}
	.composer-foot {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 10px;
		flex-wrap: wrap;
		margin-top: 10px;
	}
	.attach {
		display: flex;
		align-items: center;
		gap: 8px;
		flex-wrap: wrap;
		min-width: 0;
	}
	.attach-btn {
		padding: 8px 12px;
		font-size: 13px;
	}
	.attach-name {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		max-width: 180px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: 12px;
		color: var(--muted);
	}
	.attach-x {
		border: none;
		background: transparent;
		color: var(--muted);
		font-size: 16px;
		line-height: 1;
		padding: 0 2px;
	}
	.photo-err {
		margin: 10px 0 0;
	}
	.ci-list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 10px;
	}
	.ci-item {
		display: flex;
		gap: 12px;
		padding: 14px;
	}
	.ci-dot {
		width: 26px;
		height: 26px;
		border-radius: 999px;
		background: var(--lime);
		color: var(--ink);
		display: inline-flex;
		align-items: center;
		justify-content: center;
		flex: none;
	}
	.ci-body {
		min-width: 0;
	}
	.ci-note {
		margin: 0 0 2px;
		font-size: 14px;
	}
	.ci-photo {
		display: block;
		padding: 0;
		border: 1px solid var(--line);
		border-radius: 10px;
		overflow: hidden;
		background: var(--tag-bg);
		margin: 6px 0;
		line-height: 0;
	}
	.ci-photo img {
		display: block;
		width: 160px;
		max-width: 100%;
		height: 120px;
		object-fit: cover;
	}
	.lightbox {
		position: fixed;
		inset: 0;
		z-index: 50;
		background: rgba(20, 40, 45, 0.78);
		display: flex;
		align-items: center;
		justify-content: center;
		padding: 24px;
	}
	.lightbox img {
		max-width: 100%;
		max-height: 100%;
		border-radius: 12px;
		box-shadow: var(--shadow);
	}
	.empty-plan {
		display: flex;
		flex-direction: column;
		align-items: center;
		text-align: center;
		gap: 8px;
		padding: 34px 20px;
	}
	.small {
		font-size: 12px;
	}

	.community {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 14px 16px;
	}
	.community-ic {
		width: 34px;
		height: 34px;
		border-radius: 999px;
		background: color-mix(in srgb, var(--sage) 40%, var(--card));
		color: var(--sage-ink);
		display: inline-flex;
		align-items: center;
		justify-content: center;
		flex: none;
	}
	.community p {
		margin: 0;
		font-size: 14px;
	}
	.pledge {
		padding: 18px;
	}
	.pledge-amount {
		font-size: 28px;
		margin: 6px 0 8px;
	}
	.pledge-note {
		margin-top: 10px;
	}
	.pledge-form {
		display: flex;
		flex-direction: column;
		gap: 10px;
		margin: 14px 0 8px;
	}
	.inputrow {
		display: flex;
		align-items: center;
		gap: 8px;
		border: 1px solid var(--line);
		border-radius: 10px;
		padding: 4px 12px;
		background: var(--bg);
	}
	.cur {
		color: var(--muted);
		font-weight: 600;
	}
	.inputrow input {
		flex: 1;
		border: none;
		background: transparent;
		outline: none;
		font-size: 16px;
		font-family: inherit;
		color: var(--ink);
		width: 100%;
		min-width: 0;
	}
	.wallet-link {
		display: inline-flex;
		align-items: center;
		gap: 3px;
		font-weight: 600;
		color: var(--sky-ink);
		font-size: 13px;
	}
	.actions-card {
		padding: 8px;
		display: flex;
		flex-direction: column;
	}
	.act {
		display: flex;
		align-items: center;
		gap: 10px;
		background: transparent;
		border: none;
		text-align: left;
		padding: 12px;
		border-radius: 10px;
		font-size: 14px;
		font-weight: 600;
		color: var(--ink);
	}
	.act:hover {
		background: var(--tag-bg);
	}
	.act.danger {
		color: #c0263a;
	}
	.error {
		color: #c0263a;
		font-size: 14px;
	}

	/* plan header row with the "Adjust my plan" entry point */
	.plan-head {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 12px;
	}
	.adjust-open {
		flex: none;
		padding: 8px 12px;
		font-size: 13px;
	}

	/* planned break block in the timeline — visually distinct "away" card */
	.timeline li.brk {
		align-items: center;
	}
	.brk-node {
		width: 36px;
		height: 36px;
		border-radius: 999px;
		background: color-mix(in srgb, var(--sage) 55%, var(--card));
		border: 2px dashed var(--sage-ink);
		display: inline-flex;
		align-items: center;
		justify-content: center;
		font-size: 15px;
		flex: none;
		z-index: 1;
	}
	.brk-card {
		flex: 1;
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 8px 12px;
		background: color-mix(in srgb, var(--sage) 22%, var(--card));
		border: 1px dashed var(--sage-ink);
		border-radius: 12px;
		padding: 12px 14px;
	}
	.brk-text {
		margin: 0;
		font-size: 14px;
		font-weight: 600;
		color: var(--ink);
	}
	.brk-tag {
		display: inline-block;
		background: color-mix(in srgb, var(--sage) 45%, var(--card));
		color: var(--sage-ink);
		border-radius: 999px;
		padding: 3px 10px;
		font-size: 12px;
		font-weight: 600;
	}

	/* add-to-calendar affordances in the plan tab */
	.cal-bar {
		display: flex;
		flex-wrap: wrap;
		gap: 8px;
		margin: 0 0 18px;
	}
	.cal-all {
		padding: 8px 12px;
		font-size: 13px;
	}
	.cal-btn {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		background: transparent;
		border: 1px solid var(--line);
		border-radius: 999px;
		padding: 4px 10px;
		font-size: 12px;
		font-weight: 600;
		color: var(--sky-ink);
		margin-top: 2px;
	}
	.cal-btn:hover {
		background: var(--tag-bg);
	}

	/* adjust mini-chat modal */
	.adjust-backdrop {
		position: fixed;
		inset: 0;
		background: rgba(20, 40, 45, 0.32);
		z-index: 40;
	}
	.adjust-modal {
		position: fixed;
		z-index: 41;
		left: 50%;
		bottom: 0;
		transform: translateX(-50%);
		width: min(560px, 100%);
		max-height: min(80vh, 640px);
		display: flex;
		flex-direction: column;
		gap: 14px;
		padding: 18px;
		border-radius: 18px 18px 0 0;
	}
	.adjust-head {
		display: flex;
		align-items: flex-start;
		gap: 12px;
	}
	.adjust-badge {
		width: 36px;
		height: 36px;
		border-radius: 12px;
		background: var(--teal);
		color: var(--lime);
		display: inline-flex;
		align-items: center;
		justify-content: center;
		flex: none;
	}
	.adjust-title {
		flex: 1;
		min-width: 0;
	}
	.adjust-title h2 {
		font-size: 18px;
	}
	.adjust-title p {
		margin: 2px 0 0;
	}
	.adjust-close {
		background: transparent;
		border: none;
		color: var(--muted);
		flex: none;
		padding: 4px;
	}
	.adjust-thread {
		flex: 1;
		overflow-y: auto;
		display: flex;
		flex-direction: column;
		gap: 12px;
		padding: 2px;
	}
	.row {
		display: flex;
		gap: 10px;
		align-items: flex-end;
	}
	.row.user {
		justify-content: flex-end;
	}
	.av {
		width: 28px;
		height: 28px;
		border-radius: 999px;
		background: var(--lime);
		color: var(--ink);
		display: inline-flex;
		align-items: center;
		justify-content: center;
		flex: none;
	}
	.bubble {
		max-width: 78%;
		padding: 11px 14px;
		font-size: 14px;
		line-height: 1.5;
		white-space: pre-wrap;
		word-break: break-word;
	}
	.a-bubble {
		background: var(--card);
		border: 1px solid var(--line);
		border-radius: 4px 16px 16px 16px;
		box-shadow: var(--shadow);
	}
	.u-bubble {
		background: var(--teal);
		color: #fff;
		border-radius: 16px 16px 4px 16px;
	}
	.a-col {
		display: flex;
		flex-direction: column;
		gap: 8px;
		max-width: 82%;
		min-width: 0;
	}
	.a-col .bubble {
		max-width: 100%;
	}

	/* structured proposal preview, under a coach message */
	.proposal {
		background: var(--tag-bg);
		border: 1px solid var(--line);
		border-radius: 14px;
		padding: 12px 14px;
		display: flex;
		flex-direction: column;
		gap: 12px;
		font-size: 13px;
	}
	.prop-summary {
		margin: 0;
		font-weight: 600;
		color: var(--ink);
		line-height: 1.4;
	}
	.prop-group {
		display: flex;
		flex-direction: column;
		gap: 5px;
	}
	.prop-label {
		text-transform: uppercase;
		letter-spacing: 0.1em;
		font-size: 10px;
		font-weight: 700;
		color: var(--muted);
	}
	.prop-list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 4px;
	}
	.prop-list li {
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		gap: 6px;
		line-height: 1.4;
	}
	.prop-list strong {
		font-weight: 600;
	}
	.prop-dates {
		color: var(--muted);
		font-variant-numeric: tabular-nums;
	}
	.prop-remove {
		color: var(--muted);
	}
	.prop-badge {
		font-size: 11px;
		font-weight: 700;
		padding: 1px 7px;
		border-radius: 999px;
		background: color-mix(in srgb, var(--lime) 45%, var(--card));
		color: var(--ink);
	}
	.prop-badge-remove {
		background: var(--track);
		color: var(--muted);
	}
	.notice {
		display: flex;
		align-items: center;
		gap: 12px;
		background: color-mix(in srgb, var(--lime) 22%, var(--card));
		border: 1px solid var(--line);
		border-radius: 12px;
		padding: 12px 14px;
		font-size: 14px;
		font-weight: 500;
	}
	.notice .btn {
		margin-left: auto;
	}
	.notice-done {
		background: color-mix(in srgb, var(--lime) 35%, var(--card));
	}
	.confirm-hint {
		margin: 0;
		padding: 2px 2px 0;
	}
	.adjust-form {
		display: flex;
		gap: 10px;
		background: var(--bg);
		border: 1px solid var(--line);
		border-radius: 14px;
		padding: 8px 8px 8px 16px;
	}
	.adjust-form input {
		flex: 1;
		border: none;
		outline: none;
		background: transparent;
		font-size: 15px;
		color: var(--ink);
		font-family: inherit;
		min-width: 0;
	}
	.sendbtn {
		border: none;
		border-radius: 10px;
		background: var(--lime);
		color: var(--ink);
		width: 42px;
		height: 42px;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		flex: none;
	}

	@media (min-width: 560px) {
		.adjust-modal {
			bottom: 24px;
			border-radius: 18px;
		}
	}

	@media (max-width: 860px) {
		.layout {
			flex-direction: column;
		}
		.side {
			width: 100%;
		}
	}
</style>
