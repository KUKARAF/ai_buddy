<script lang="ts">
	import { resolve } from '$app/paths';
	import Icon from '$lib/components/Icon.svelte';
	import GoalCard from '$lib/components/GoalCard.svelte';
	import { app } from '$lib/appState.svelte';
	import { getRoadmap, type Roadmap } from '$lib/api/client';
	import { shortDate } from '$lib/format';
	import { goalLook } from '$lib/goalView';
	import { wizard } from '$lib/wizardState.svelte';

	let progress = $state<Record<string, { done: number; total: number }>>({});
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
					progress = {
						...progress,
						[goal.id]: {
							done: steps.filter((s) => s.status === 'done').length,
							total: steps.length
						}
					};
				})
				.catch(() => {
					progress = { ...progress, [goal.id]: { done: 0, total: 0 } };
				});
		}
	});

	const FILTERS = ['All goals', 'Active', 'Completed', 'Paused'] as const;
	type Filter = (typeof FILTERS)[number];
	let filter = $state<Filter>('All goals');

	function inFilter(status: string, f: Filter): boolean {
		switch (f) {
			case 'Active':
				return status === 'active' || status === 'draft';
			case 'Completed':
				return status === 'succeeded';
			case 'Paused':
				return status === 'abandoned' || status === 'failed';
			default:
				return true;
		}
	}

	const filtered = $derived(app.goals.filter((g) => inFilter(g.status, filter)));

	function countFor(f: Filter): number {
		return app.goals.filter((g) => inFilter(g.status, f)).length;
	}
</script>

<div class="page">
	<div class="head">
		<h1>My goals</h1>
		<button class="btn" onclick={() => wizard.openWizard()}
			><Icon name="plus" size={16} /> New goal</button
		>
	</div>

	{#if app.isGuest}
		<div class="card empty">
			<span class="tile tile-sky"><Icon name="lock" size={24} /></span>
			<h2>Sign in to see your goals</h2>
			<p class="muted">Your goals are private to you. Sign in, or start shaping one now.</p>
			<button class="btn btn-lime" onclick={() => wizard.openWizard()}>
				<Icon name="plus" size={16} /> New goal
			</button>
		</div>
	{:else if app.loaded && app.goals.length === 0}
		<div class="card empty">
			<span class="tile tile-sage"><Icon name="target" size={24} /></span>
			<h2>No goals yet</h2>
			<p class="muted">Start your first one with the coach — it only takes a small conversation.</p>
			<button class="btn btn-lime" onclick={() => wizard.openWizard()}>
				<Icon name="plus" size={16} /> New goal
			</button>
		</div>
	{:else}
		<div class="tabs" role="tablist">
			{#each FILTERS as f (f)}
				<button
					class="tab"
					class:on={filter === f}
					role="tab"
					aria-selected={filter === f}
					onclick={() => (filter = f)}
				>
					{f}
					<span class="tab-count">{countFor(f)}</span>
				</button>
			{/each}
		</div>

		<div class="grid">
			<button class="card add-tile" onclick={() => wizard.openWizard()}>
				<span class="add-plus"><Icon name="plus" size={22} /></span>
				<strong>What's your next someday?</strong>
				<span class="muted small">Start a new goal</span>
			</button>

			{#each filtered as goal (goal.id)}
				{@const look = goalLook(goal)}
				{@const p = progress[goal.id]}
				<GoalCard
					title={goal.title}
					category={look.category}
					tile={look.tile}
					icon={look.icon}
					deadline={shortDate(goal.deadline)}
					done={p?.done ?? 0}
					total={p?.total ?? 0}
					href={resolve('/goals/[id]', { id: goal.id })}
				/>
			{/each}
		</div>

		{#if filtered.length === 0}
			<p class="muted none">No goals in this view yet.</p>
		{/if}
	{/if}
</div>

<style>
	.page {
		display: flex;
		flex-direction: column;
		gap: 20px;
	}
	.head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 16px;
	}
	.head h1 {
		font-size: 30px;
	}
	.tabs {
		display: flex;
		gap: 6px;
		flex-wrap: wrap;
	}
	.tab {
		display: inline-flex;
		align-items: center;
		gap: 7px;
		background: transparent;
		border: 1px solid var(--line);
		border-radius: 999px;
		padding: 7px 14px;
		font-size: 13px;
		font-weight: 600;
		color: var(--muted);
	}
	.tab.on {
		background: var(--teal);
		border-color: var(--teal);
		color: #fff;
	}
	.tab-count {
		font-size: 11px;
		opacity: 0.7;
	}
	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
		gap: 16px;
	}
	.add-tile {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 8px;
		text-align: center;
		border-style: dashed;
		color: var(--ink);
		min-height: 200px;
		padding: 20px;
	}
	.add-tile:hover {
		border-color: var(--lime);
	}
	.add-plus {
		width: 46px;
		height: 46px;
		border-radius: 12px;
		background: color-mix(in srgb, var(--lime) 30%, var(--card));
		display: inline-flex;
		align-items: center;
		justify-content: center;
		color: var(--ink);
	}
	.small {
		font-size: 12px;
	}
	.none {
		text-align: center;
		padding: 20px;
	}
	.empty {
		display: flex;
		flex-direction: column;
		align-items: center;
		text-align: center;
		gap: 12px;
		padding: 48px 24px;
		max-width: 460px;
		margin: 20px auto;
	}
	.empty h2 {
		font-size: 20px;
	}
	.empty p {
		margin: 0;
		max-width: 40ch;
	}
	.empty .btn {
		margin-top: 6px;
	}
</style>
