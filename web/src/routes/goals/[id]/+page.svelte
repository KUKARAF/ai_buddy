<script lang="ts">
	import { onMount } from 'svelte';
	import { page } from '$app/state';
	import { resolve } from '$app/paths';
	import { goto } from '$app/navigation';
	import Icon from '$lib/components/Icon.svelte';
	import { app } from '$lib/appState.svelte';
	import { shortDate, relativeDate, euros } from '$lib/format';
	import { goalLook } from '$lib/goalView';
	import {
		getGoal,
		getRoadmap,
		listGoalCheckIns,
		getPledge,
		patchStep,
		patchGoal,
		createGoalCheckIn,
		createPledge,
		confirmPledge,
		getWallet,
		getSimilar,
		ApiError,
		type Goal,
		type Roadmap,
		type RoadmapStep,
		type CheckIn,
		type Pledge,
		type SimilarGoals
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
		try {
			const ci = await createGoalCheckIn(goalId, { note: text });
			checkIns = [ci, ...checkIns];
			note = '';
			composerOpen = false;
			void app.refreshStreak();
		} catch {
			/* ignore; keep the text so the user can retry */
		} finally {
			checkinSubmitting = false;
		}
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
						<h2>Small steps. Real progress.</h2>
						<p class="muted sub">A starter framework you control — tap a step to mark it done.</p>

						{#if steps.length === 0}
							<div class="card empty-plan">
								<p class="muted">Your plan isn't ready yet.</p>
								<p class="muted small">We'll build your milestones when coaching is available.</p>
							</div>
						{:else}
							<ol class="timeline">
								{#each steps as step, i (step.id)}
									<li class:done={step.status === 'done'}>
										<button
											class="node"
											aria-label={step.status === 'done' ? 'Mark step not done' : 'Mark step done'}
											onclick={() => toggleStep(step)}
										>
											{#if step.status === 'done'}
												<Icon name="check" size={15} stroke={2.6} />
											{:else}
												{String(i + 1).padStart(2, '0')}
											{/if}
										</button>
										<div class="ms-body">
											<p class="eyebrow">Milestone {i + 1}</p>
											<h3>{step.title}</h3>
											{#if step.detail}<p class="muted">{step.detail}</p>{/if}
											<span class="chip-status" class:completed={step.status === 'done'}>
												{#if step.status === 'done'}
													Completed
												{:else}
													{goal.time_per_session_min ?? 20} min / session
												{/if}
											</span>
										</div>
									</li>
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
								<div class="composer-foot">
									<button
										type="submit"
										class="btn"
										disabled={checkinSubmitting || note.trim() === ''}
									>
										{checkinSubmitting ? 'Saving…' : 'Save check-in'}
									</button>
								</div>
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
										<div>
											<p class="ci-note">{ci.note ?? 'Checked in.'}</p>
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
		justify-content: flex-end;
		margin-top: 10px;
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
	.ci-note {
		margin: 0 0 2px;
		font-size: 14px;
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

	@media (max-width: 860px) {
		.layout {
			flex-direction: column;
		}
		.side {
			width: 100%;
		}
	}
</style>
