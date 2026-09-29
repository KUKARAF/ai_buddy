<script lang="ts">
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import Icon from './Icon.svelte';
	import { wizard } from '$lib/wizardState.svelte';
	import { app } from '$lib/appState.svelte';
	import { createGoal, generateRoadmap, ApiError, type Goal, type Roadmap } from '$lib/api/client';
	import { CATEGORIES } from '$lib/goalView';

	const MAX = 300;
	const INSPIRATION = [
		'Sing a song at my wedding',
		'Have a conversation in Spanish',
		'Learn to juggle three balls'
	];
	const SKILLS = ['Beginner', 'Intermediate', 'Advanced'];
	const TIMES = [10, 20, 30, 45];

	let step = $state(1);
	let someday = $state('');
	let deadline = $state('');
	let location = $state('');
	let skill = $state('Beginner');
	let category = $state('Creative');
	let timePer = $state(20);
	let criterion = $state('');

	// step 4 build state
	let building = $state(false);
	let createdGoal = $state<Goal | null>(null);
	let roadmap = $state<Roadmap | null>(null);
	let planError = $state<{ kind: 'wallet' | 'other'; message: string } | null>(null);
	let createError = $state<string | null>(null);

	// Reset whenever the wizard is (re)opened, applying any prefill.
	let wasOpen = false;
	$effect(() => {
		if (wizard.open && !wasOpen) {
			step = 1;
			someday = wizard.prefill?.title ?? '';
			deadline = '';
			location = '';
			skill = 'Beginner';
			category = wizard.prefill?.category ?? 'Creative';
			timePer = 20;
			criterion = '';
			building = false;
			createdGoal = null;
			roadmap = null;
			planError = null;
			createError = null;
		}
		wasOpen = wizard.open;
	});

	const title = $derived(deriveTitle(someday));
	function deriveTitle(text: string): string {
		const first = text
			.trim()
			.split(/[\n.!?]/)[0]
			.trim();
		const base = first.length > 0 ? first : text.trim();
		return base.length > 80 ? `${base.slice(0, 77).trimEnd()}…` : base;
	}

	function chooseInspiration(text: string): void {
		someday = text;
	}

	function next(): void {
		if (step < 4) step += 1;
		if (step === 4) void buildPlan();
	}
	function back(): void {
		if (step > 1) step -= 1;
	}

	async function buildPlan(): Promise<void> {
		if (createdGoal) return; // already built
		building = true;
		createError = null;
		planError = null;
		try {
			const goal = await createGoal({
				title,
				description: someday.trim() || title,
				category,
				...(deadline ? { deadline } : {}),
				...(location.trim() ? { location: location.trim() } : {}),
				skill_level: skill,
				...(criterion.trim() ? { success_criterion: criterion.trim() } : {}),
				time_per_session_min: timePer
			});
			createdGoal = goal;
			void app.refreshGoals();
			try {
				roadmap = await generateRoadmap(goal.id);
			} catch (err) {
				if (err instanceof ApiError && err.status === 402) {
					planError = { kind: 'wallet', message: 'Your wallet is empty.' };
				} else {
					planError = {
						kind: 'other',
						message:
							err instanceof ApiError
								? err.message
								: 'The plan will be ready when coaching is available.'
					};
				}
			}
		} catch (err) {
			createError = err instanceof ApiError ? err.message : 'Could not create your goal.';
		} finally {
			building = false;
		}
	}

	function finish(): void {
		if (!createdGoal) return;
		const id = createdGoal.id;
		wizard.close();
		void goto(resolve('/goals/[id]', { id }));
	}

	function onKeydown(e: KeyboardEvent): void {
		if (e.key === 'Escape') wizard.close();
	}
</script>

<svelte:window onkeydown={onKeydown} />

{#if wizard.open}
	<div class="overlay">
		<button class="scrim" aria-label="Close" onclick={() => wizard.close()}></button>
		<div class="modal card" role="dialog" aria-modal="true" aria-label="New goal">
			<button class="close" aria-label="Close" onclick={() => wizard.close()}>
				<Icon name="x" size={18} />
			</button>

			<div class="progress4">
				{#each [1, 2, 3, 4] as s (s)}
					<span class="seg" class:on={s <= step}></span>
				{/each}
			</div>
			<div class="wz-head">
				<span class="badge"><Icon name="check" size={16} stroke={2.4} /></span>
				<span class="eyebrow">A new beginning &nbsp; 0{step} / 04</span>
			</div>

			<!-- STEP 1 -->
			{#if step === 1}
				<h2>What's your someday?</h2>
				<p class="muted sub">No perfect words needed. Tell us what you'd love to make happen.</p>
				<div class="ta-wrap">
					<textarea
						maxlength={MAX}
						bind:value={someday}
						placeholder="I've always wanted to…"
						rows="4"></textarea>
					<div class="ta-foot">
						<button class="ramble" type="button" disabled title="Voice input is coming soon">
							<Icon name="mic" size={15} /> Ramble mode
						</button>
						<span class="count">{someday.length}/{MAX}</span>
					</div>
				</div>
				<p class="eyebrow chips-label">Need a little inspiration?</p>
				<div class="chips">
					{#each INSPIRATION as idea (idea)}
						<button type="button" class="chip" onclick={() => chooseInspiration(idea)}
							>{idea}</button
						>
					{/each}
				</div>
				<div class="actions">
					<button class="btn btn-ghost" onclick={() => wizard.close()}>Not just yet</button>
					<button class="btn" disabled={someday.trim() === ''} onclick={next}>Continue</button>
				</div>

				<!-- STEP 2 -->
			{:else if step === 2}
				<h2>Let's make it yours.</h2>
				<p class="muted sub">A realistic plan starts with where you are, and the time you have.</p>
				<div class="fields">
					<label>
						<span>When's your finish line?</span>
						<input type="date" bind:value={deadline} />
					</label>
					<label>
						<span>Where will you work on it?</span>
						<input type="text" bind:value={location} placeholder="Anywhere, or your city" />
					</label>
					<label>
						<span>Your starting point</span>
						<select bind:value={skill}>
							{#each SKILLS as s (s)}<option value={s}>{s}</option>{/each}
						</select>
					</label>
					<label>
						<span>Goal category</span>
						<select bind:value={category}>
							{#each CATEGORIES as c (c)}<option value={c}>{c}</option>{/each}
						</select>
					</label>
					<label>
						<span>Time for a little practice</span>
						<select bind:value={timePer}>
							{#each TIMES as t (t)}<option value={t}>{t} min</option>{/each}
						</select>
					</label>
				</div>
				<div class="actions">
					<button class="btn btn-ghost" onclick={back}
						><Icon name="chevron-l" size={15} /> Back</button
					>
					<button class="btn" onclick={next}>Continue</button>
				</div>

				<!-- STEP 3 -->
			{:else if step === 3}
				<h2>What will "I did it" look like?</h2>
				<p class="muted sub">Make the finish line something you can see, do, or record.</p>
				<div class="goal-echo pill">{title}</div>
				<div class="fields">
					<label>
						<span>I'll know I've finished when…</span>
						<textarea
							rows="3"
							bind:value={criterion}
							placeholder="e.g. I record myself singing the whole song, in tune."></textarea>
					</label>
				</div>
				<div class="actions">
					<button class="btn btn-ghost" onclick={back}
						><Icon name="chevron-l" size={15} /> Back</button
					>
					<button class="btn" onclick={next}>Build my starter plan</button>
				</div>

				<!-- STEP 4 -->
			{:else}
				<h2>Your someday has a first step.</h2>
				{#if building}
					<div class="loading">
						<span class="spinner"></span>
						<p class="muted">Building your starter plan…</p>
					</div>
				{:else if createError}
					<p class="error">{createError}</p>
					<div class="actions">
						<button class="btn btn-ghost" onclick={back}
							><Icon name="chevron-l" size={15} /> Back</button
						>
						<button class="btn" onclick={() => buildPlan()}>Try again</button>
					</div>
				{:else if createdGoal}
					<p class="meta muted">
						{timePer} min · {deadline || 'no deadline yet'} · {skill}
					</p>

					{#if roadmap && roadmap.steps.length > 0}
						<ol class="plan">
							{#each roadmap.steps as s, i (s.id)}
								<li>
									<span class="num">{String(i + 1).padStart(2, '0')}</span>
									<span class="plan-body">
										<strong>{s.title}</strong>
										{#if s.detail}<span class="muted">{s.detail}</span>{/if}
									</span>
								</li>
							{/each}
						</ol>
					{:else if planError}
						<div class="notice">
							<p><strong>Your goal is saved.</strong></p>
							<p class="muted">
								{#if planError.kind === 'wallet'}
									We'll build the plan when your wallet can cover coaching.
								{:else}
									We'll build the plan when coaching is available.
								{/if}
							</p>
							{#if planError.kind === 'wallet'}
								<a class="wallet-link" href={resolve('/wallet')} onclick={() => wizard.close()}>
									Top up your wallet <Icon name="chevron-r" size={14} />
								</a>
							{/if}
						</div>
					{/if}

					<p class="private-note muted">
						<Icon name="lock" size={13} /> Private by default. No pledge required to begin.
					</p>
					<div class="actions">
						<button class="btn btn-ghost" onclick={back}
							><Icon name="chevron-l" size={15} /> Back</button
						>
						<button class="btn btn-lime" onclick={finish}>Let's make it happen</button>
					</div>
				{/if}
			{/if}
		</div>
	</div>
{/if}

<style>
	.overlay {
		position: fixed;
		inset: 0;
		z-index: 60;
		display: flex;
		align-items: flex-start;
		justify-content: center;
		padding: 40px 16px;
		overflow-y: auto;
	}
	.scrim {
		position: fixed;
		inset: 0;
		border: none;
		background: rgba(20, 40, 45, 0.4);
		backdrop-filter: blur(2px);
	}
	.modal {
		position: relative;
		z-index: 1;
		width: 100%;
		max-width: 520px;
		padding: 26px 28px 24px;
		border-radius: 20px;
	}
	.close {
		position: absolute;
		top: 16px;
		right: 16px;
		background: transparent;
		border: none;
		color: var(--muted);
		border-radius: 8px;
		padding: 4px;
	}
	.progress4 {
		display: flex;
		gap: 6px;
		margin-bottom: 16px;
	}
	.seg {
		flex: 1;
		height: 5px;
		border-radius: 999px;
		background: var(--track);
	}
	.seg.on {
		background: var(--lime);
	}
	.wz-head {
		display: flex;
		align-items: center;
		gap: 10px;
		margin-bottom: 14px;
	}
	.badge {
		width: 30px;
		height: 30px;
		border-radius: 9px;
		background: var(--teal);
		color: var(--lime);
		display: inline-flex;
		align-items: center;
		justify-content: center;
		flex: none;
	}
	h2 {
		font-size: 23px;
		margin-bottom: 6px;
	}
	.sub {
		font-size: 14px;
		margin: 0 0 18px;
	}
	.ta-wrap {
		border: 1px solid var(--line);
		border-radius: 14px;
		padding: 12px;
		background: var(--bg);
	}
	textarea,
	input,
	select {
		font-family: inherit;
		font-size: 15px;
		color: var(--ink);
		width: 100%;
		border: none;
		background: transparent;
		outline: none;
		resize: vertical;
	}
	.ta-foot {
		display: flex;
		align-items: center;
		justify-content: space-between;
		margin-top: 8px;
	}
	.ramble {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		background: var(--tag-bg);
		border: 1px solid var(--line);
		border-radius: 999px;
		padding: 6px 12px;
		font-size: 13px;
		font-weight: 600;
		color: var(--muted);
	}
	.count {
		font-size: 12px;
		color: var(--muted);
	}
	.chips-label {
		margin: 18px 0 10px;
	}
	.chips {
		display: flex;
		flex-wrap: wrap;
		gap: 8px;
	}
	.chip {
		background: var(--tag-bg);
		border: 1px solid var(--line);
		border-radius: 999px;
		padding: 8px 14px;
		font-size: 13px;
		font-weight: 600;
		color: var(--ink);
	}
	.chip:hover {
		border-color: var(--lime);
	}
	.fields {
		display: flex;
		flex-direction: column;
		gap: 14px;
	}
	label {
		display: flex;
		flex-direction: column;
		gap: 6px;
	}
	label > span {
		font-size: 13px;
		font-weight: 600;
		color: var(--muted);
	}
	.fields input,
	.fields select,
	.fields textarea {
		border: 1px solid var(--line);
		border-radius: 12px;
		padding: 11px 12px;
		background: var(--bg);
	}
	.goal-echo {
		display: inline-block;
		margin-bottom: 16px;
		color: var(--ink);
		background: color-mix(in srgb, var(--lime) 22%, var(--card));
	}
	.actions {
		display: flex;
		justify-content: space-between;
		gap: 12px;
		margin-top: 24px;
	}
	.actions .btn {
		flex: 1;
		justify-content: center;
	}
	.loading {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 14px;
		padding: 40px 0;
	}
	.spinner {
		width: 34px;
		height: 34px;
		border-radius: 999px;
		border: 3px solid var(--track);
		border-top-color: var(--lime);
		animation: spin 0.8s linear infinite;
	}
	@keyframes spin {
		to {
			transform: rotate(360deg);
		}
	}
	.meta {
		font-size: 13px;
		margin: 0 0 16px;
	}
	.plan {
		list-style: none;
		margin: 0 0 16px;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 12px;
	}
	.plan li {
		display: flex;
		gap: 12px;
		align-items: flex-start;
	}
	.num {
		width: 30px;
		height: 30px;
		border-radius: 999px;
		background: var(--tag-bg);
		color: var(--ink);
		font-size: 12px;
		font-weight: 700;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		flex: none;
	}
	.plan-body {
		display: flex;
		flex-direction: column;
		gap: 2px;
		font-size: 14px;
	}
	.plan-body .muted {
		font-size: 13px;
	}
	.notice {
		background: var(--tag-bg);
		border: 1px solid var(--line);
		border-radius: 12px;
		padding: 14px;
		margin-bottom: 16px;
	}
	.notice p {
		margin: 0 0 4px;
		font-size: 14px;
	}
	.wallet-link {
		display: inline-flex;
		align-items: center;
		gap: 3px;
		font-weight: 600;
		color: var(--sky-ink);
		font-size: 13px;
		margin-top: 4px;
	}
	.private-note {
		display: flex;
		align-items: center;
		gap: 6px;
		font-size: 13px;
		margin: 0;
	}
	.error {
		color: #c0263a;
		font-size: 14px;
	}
</style>
