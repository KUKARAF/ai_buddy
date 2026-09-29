<script lang="ts">
	import { resolve } from '$app/paths';
	import Icon from '$lib/components/Icon.svelte';
	import GoalCard from '$lib/components/GoalCard.svelte';
	import { app } from '$lib/appState.svelte';
	import { getRoadmap, type Roadmap, type RoadmapStep } from '$lib/api/client';
	import { dateEyebrow, shortDate } from '$lib/format';
	import { goalLook } from '$lib/goalView';
	import { wizard } from '$lib/wizardState.svelte';

	const WEEK_LABELS = ['M', 'T', 'W', 'T', 'F', 'S', 'S'];

	// Best-effort per-goal roadmap progress, keyed by goal id.
	interface Progress {
		done: number;
		total: number;
		firstPending: RoadmapStep | null;
	}
	let roadmaps = $state<Record<string, Progress>>({});
	// Non-reactive guard so each goal's roadmap is fetched at most once.
	const started: Record<string, true> = {};

	$effect(() => {
		if (!app.loaded) return;
		for (const goal of app.goals) {
			if (started[goal.id]) continue;
			started[goal.id] = true;
			getRoadmap(goal.id)
				.then((r: Roadmap) => {
					const steps = r.steps ?? [];
					const done = steps.filter((s) => s.status === 'done').length;
					const firstPending = steps.find((s) => s.status === 'pending') ?? null;
					roadmaps = { ...roadmaps, [goal.id]: { done, total: steps.length, firstPending } };
				})
				.catch(() => {
					roadmaps = { ...roadmaps, [goal.id]: { done: 0, total: 0, firstPending: null } };
				});
		}
	});

	const today = new Date();
	const eyebrow = dateEyebrow(today);

	// Hero: from the first pending step of the first goal's roadmap, else example.
	const heroGoal = $derived(app.goals[0] ?? null);
	const heroStep = $derived(heroGoal ? (roadmaps[heroGoal.id]?.firstPending ?? null) : null);
	const heroTitle = $derived(heroStep?.title ?? 'Build a comfortable warm-up');
	const heroDesc = $derived(
		heroStep?.detail ??
			(heroGoal
				? `A small step toward "${heroGoal.title}".`
				: 'Ten quiet minutes. Loosen up, hum the melody, and let it feel easy before it feels good.')
	);

	// Example goal cards for guests / empty state.
	const exampleGoals = [
		{
			title: 'Sing our song at my wedding',
			category: 'Creative',
			tile: 'peach' as const,
			icon: 'music',
			deadline: '12 Dec',
			done: 1,
			total: 4
		},
		{
			title: 'Have a real conversation in Spanish',
			category: 'Learning',
			tile: 'lavender' as const,
			icon: 'book',
			deadline: '30 Nov',
			done: 2,
			total: 4
		}
	];

	const showBanner = $derived(app.showExamples);

	// Streak (real when signed in; otherwise an example week).
	const streak = $derived(app.streak);
	const streakLabel = $derived.by(() => {
		const n = streak?.current_streak ?? 0;
		return n > 0 ? `${n} day streak` : 'Your streak starts today';
	});
	const checkInCount = $derived(streak ? streak.week.filter((d) => d.checked).length : 2);

	// A 7-cell view model: real streak days, or an example (first two checked).
	interface DayCell {
		label: string;
		checked: boolean;
		today: boolean;
		num: number;
	}
	const weekCells = $derived.by((): DayCell[] => {
		if (streak) {
			return streak.week.map((d, i) => {
				const date = new Date(d.date);
				const dow = (date.getDay() + 6) % 7; // Mon=0
				return {
					label: WEEK_LABELS[Number.isNaN(dow) ? i : dow],
					checked: d.checked,
					today: i === streak.week.length - 1,
					num: Number.isNaN(date.getDate()) ? i + 1 : date.getDate()
				};
			});
		}
		return WEEK_LABELS.map((label, i) => ({
			label,
			checked: i < 2,
			today: i === 2,
			num: i + 24
		}));
	});

	function goalHref(id: string): string {
		return resolve('/goals/[id]', { id });
	}
</script>

<div class="today">
	<div class="col-main">
		{#if showBanner}
			<div class="banner">
				<span>You're exploring an example workspace. Make it yours with your first goal.</span>
				<button class="banner-cta" onclick={() => wizard.openWizard()}>
					Start my goal <Icon name="plus" size={13} />
				</button>
			</div>
		{/if}

		<div class="greet">
			<div>
				<p class="eyebrow">{eyebrow}</p>
				<h1>A little closer, {app.firstName}<span class="dot-lime">.</span></h1>
				<p class="muted sub">You don't have to do it all. Just the next small thing.</p>
			</div>
			<span class="streak">
				<Icon name="flame" size={16} />
				<span>{streakLabel}</span>
			</span>
		</div>

		<!-- HERO -->
		<section class="hero">
			<Icon name="arrow-ur" size={190} stroke={1} />
			<div class="hero-body">
				<div class="hero-top">
					<span class="hero-eyebrow"><span class="reddot"></span> Your next small step</span>
					<span class="timepill">
						<Icon name="clock" size={13} />
						{heroGoal?.time_per_session_min ?? 20} min
					</span>
				</div>
				<h2 class="hero-title">{heroTitle}</h2>
				<p class="hero-desc">{heroDesc}</p>
				<div class="hero-actions">
					{#if heroGoal}
						<a class="btn btn-lime" href={resolve('/goals/[id]', { id: heroGoal.id })}
							><Icon name="check" size={16} stroke={2.4} /> Let's do this</a
						>
						<a class="see" href={resolve('/goals/[id]', { id: heroGoal.id })}
							>See the whole plan <Icon name="chevron-r" size={15} /></a
						>
					{:else}
						<button class="btn btn-lime" onclick={() => wizard.openWizard()}
							><Icon name="check" size={16} stroke={2.4} /> Let's do this</button
						>
						<a class="see" href={resolve('/goals')}
							>See the whole plan <Icon name="chevron-r" size={15} /></a
						>
					{/if}
				</div>
			</div>
		</section>

		<!-- YOUR GOALS -->
		<section class="goals-section">
			<div class="sec-head">
				<h2>
					Your goals <span class="muted"
						>{app.showExamples ? exampleGoals.length : app.goals.length}</span
					>
				</h2>
				<a class="viewall" href={resolve('/goals')}>View all <Icon name="chevron-r" size={14} /></a>
			</div>
			<div class="goal-row">
				{#if app.showExamples}
					{#each exampleGoals as g (g.title)}
						<GoalCard {...g} />
					{/each}
				{:else}
					{#each app.goals as goal (goal.id)}
						{@const look = goalLook(goal)}
						{@const p = roadmaps[goal.id]}
						<GoalCard
							title={goal.title}
							category={look.category}
							tile={look.tile}
							icon={look.icon}
							deadline={shortDate(goal.deadline)}
							done={p?.done ?? 0}
							total={p?.total ?? 0}
							href={goalHref(goal.id)}
						/>
					{/each}
				{/if}
			</div>
		</section>

		<!-- GENTLE REMINDER -->
		<section class="reminder">
			<div>
				<p class="eyebrow">A gentle reminder</p>
				<p class="reminder-text">
					Consistency isn't doing it perfectly. It's coming back, even after a messy day.
				</p>
			</div>
			<span class="counter muted">01 / 07</span>
		</section>
	</div>

	<!-- RIGHT RAIL -->
	<aside class="rail">
		<section class="card rail-card">
			<div class="rail-head">
				<Icon name="calendar" size={16} />
				<h3>Your week, in small wins</h3>
			</div>
			<p class="muted small">
				{streak ? 'Every check-in is a small win.' : 'An example of a week with momentum.'}
			</p>
			<div class="week">
				{#each weekCells as cell, i (i)}
					<div class="day">
						<span class="dl">{cell.label}</span>
						<span class="dc" class:filled={cell.checked} class:ring={cell.today && !cell.checked}>
							{#if cell.checked}<Icon name="check" size={13} stroke={2.6} />{:else}{cell.num}{/if}
						</span>
					</div>
				{/each}
			</div>
			<div class="rail-foot">
				<span class="muted small"><span class="reddot"></span> Showing up is the win.</span>
				<span class="small">{checkInCount} check-ins</span>
			</div>
		</section>

		<section class="card rail-card together">
			<p class="eyebrow">Better together</p>
			<div class="avatars">
				<span class="av tile-peach"></span>
				<span class="av tile-lavender"></span>
				<span class="av tile-sky"></span>
			</div>
			<h3>Your kind of people. Your kind of goals.</h3>
			<p class="muted small">Find a circle of people taking the same small steps as you.</p>
			<a class="btn togbtn" href={resolve('/circles')}
				><Icon name="people" size={16} /> Find your circle</a
			>
			<div class="tog-divider"></div>
			<p class="pledge-line">Put a little belief behind it.</p>
			<a class="pledge-link" href={resolve('/wallet')}
				>Discover how pledges will work <Icon name="chevron-r" size={14} /></a
			>
		</section>
	</aside>
</div>

<style>
	.today {
		display: flex;
		gap: 28px;
		align-items: flex-start;
	}
	.col-main {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 22px;
	}

	.banner {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
		font-size: 13px;
		color: var(--muted);
		background: var(--tag-bg);
		border: 1px solid var(--line);
		border-radius: 12px;
		padding: 10px 14px;
	}
	.banner-cta {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		font-weight: 600;
		color: var(--ink);
		white-space: nowrap;
		background: transparent;
		border: none;
		font-size: 13px;
		padding: 0;
	}

	.greet {
		display: flex;
		justify-content: space-between;
		align-items: flex-start;
		gap: 16px;
	}
	.greet h1 {
		font-size: 40px;
		font-weight: 600;
		line-height: 1.08;
		margin: 6px 0 8px;
		letter-spacing: -0.02em;
	}
	.sub {
		font-size: 15px;
		margin: 0;
	}
	.streak {
		display: inline-flex;
		align-items: center;
		gap: 7px;
		background: var(--card);
		border: 1px solid var(--line);
		border-radius: 999px;
		padding: 8px 14px;
		font-size: 13px;
		color: #c2560f;
		box-shadow: var(--shadow);
		white-space: nowrap;
		flex: none;
		font-weight: 600;
	}

	/* hero */
	.hero {
		position: relative;
		overflow: hidden;
		background: var(--teal);
		color: #fff;
		border-radius: 20px;
		padding: 30px;
	}
	.hero :global(svg) {
		position: absolute;
		right: -30px;
		bottom: -40px;
		color: rgba(217, 250, 117, 0.12);
	}
	.hero-body {
		position: relative;
		z-index: 1;
	}
	.hero-top {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
	}
	.hero-eyebrow {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		text-transform: uppercase;
		letter-spacing: 0.12em;
		font-size: 11px;
		font-weight: 600;
		color: rgba(255, 255, 255, 0.7);
	}
	.reddot {
		width: 7px;
		height: 7px;
		border-radius: 999px;
		background: var(--lime);
		display: inline-block;
	}
	.timepill {
		display: inline-flex;
		align-items: center;
		gap: 5px;
		background: rgba(255, 255, 255, 0.12);
		border-radius: 999px;
		padding: 5px 11px;
		font-size: 12px;
	}
	.hero-title {
		font-size: 32px;
		font-weight: 600;
		line-height: 1.12;
		margin: 16px 0 10px;
		max-width: 22ch;
	}
	.hero-desc {
		color: rgba(255, 255, 255, 0.72);
		font-size: 15px;
		margin: 0 0 22px;
		max-width: 46ch;
	}
	.hero-actions {
		display: flex;
		align-items: center;
		gap: 16px;
		flex-wrap: wrap;
	}
	.see {
		display: inline-flex;
		align-items: center;
		gap: 3px;
		color: rgba(255, 255, 255, 0.85);
		font-weight: 600;
		font-size: 14px;
	}

	/* goals */
	.sec-head {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
	}
	.sec-head h2 {
		font-size: 19px;
	}
	.viewall {
		display: inline-flex;
		align-items: center;
		gap: 3px;
		font-weight: 600;
		font-size: 14px;
	}
	.goal-row {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
		gap: 16px;
		margin-top: 14px;
	}

	/* reminder */
	.reminder {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 16px;
		background: var(--card);
		border: 1px solid var(--line);
		border-left: 4px solid var(--lime);
		border-radius: 14px;
		padding: 16px 20px;
		box-shadow: var(--shadow);
	}
	.reminder-text {
		margin: 4px 0 0;
		font-size: 15px;
		font-weight: 500;
		max-width: 60ch;
	}
	.counter {
		font-size: 13px;
		font-weight: 600;
		white-space: nowrap;
	}

	/* right rail */
	.rail {
		width: 340px;
		flex: none;
		display: flex;
		flex-direction: column;
		gap: 18px;
		position: sticky;
		top: 88px;
	}
	.rail-card {
		padding: 18px;
	}
	.rail-head {
		display: flex;
		align-items: center;
		gap: 8px;
	}
	.rail-head h3 {
		font-size: 15px;
	}
	.small {
		font-size: 12px;
	}
	.week {
		display: grid;
		grid-template-columns: repeat(7, 1fr);
		gap: 6px;
		margin: 14px 0;
	}
	.day {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 6px;
	}
	.dl {
		font-size: 11px;
		color: var(--muted);
		font-weight: 600;
	}
	.dc {
		width: 30px;
		height: 30px;
		border-radius: 999px;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		font-size: 12px;
		font-weight: 600;
		background: var(--track);
		color: var(--muted);
	}
	.dc.filled {
		background: var(--lime);
		color: var(--ink);
	}
	.dc.ring {
		background: transparent;
		border: 2px solid var(--lime);
		color: var(--ink);
	}
	.rail-foot {
		display: flex;
		align-items: center;
		justify-content: space-between;
	}
	.rail-foot .reddot {
		margin-right: 4px;
	}

	.together {
		background: color-mix(in srgb, var(--sage) 30%, var(--card));
	}
	.avatars {
		display: flex;
		margin: 12px 0 10px;
	}
	.av {
		width: 34px;
		height: 34px;
		border-radius: 999px;
		border: 2px solid var(--card);
		margin-left: -8px;
	}
	.av:first-child {
		margin-left: 0;
	}
	.together h3 {
		font-size: 16px;
		line-height: 1.25;
	}
	.togbtn {
		margin-top: 14px;
		width: 100%;
		justify-content: center;
	}
	.tog-divider {
		height: 1px;
		background: var(--line);
		margin: 16px 0 12px;
	}
	.pledge-line {
		margin: 0 0 2px;
		font-weight: 600;
		font-size: 14px;
	}
	.pledge-link {
		display: inline-flex;
		align-items: center;
		gap: 3px;
		font-size: 13px;
		font-weight: 600;
		color: var(--sky-ink);
	}

	@media (max-width: 1100px) {
		.rail {
			display: none;
		}
	}
	@media (max-width: 620px) {
		.greet h1 {
			font-size: 30px;
		}
		.hero-title {
			font-size: 26px;
		}
		.goal-row {
			grid-template-columns: 1fr;
		}
	}
</style>
