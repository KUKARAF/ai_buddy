<script lang="ts">
	import { onMount } from 'svelte';
	import Icon from '$lib/components/Icon.svelte';
	import { categoryLook } from '$lib/goalView';
	import { getCircleSuggestions, ApiError, type CircleSuggestion } from '$lib/api/client';
	import { startLogin } from '$lib/app/login';

	let circles = $state<CircleSuggestion[]>([]);
	let loading = $state(true);
	let guest = $state(false);
	let loadError = $state<string | null>(null);

	onMount(async () => {
		try {
			const res = await getCircleSuggestions();
			circles = res.circles ?? [];
		} catch (err) {
			if (err instanceof ApiError && err.status === 401) {
				guest = true;
			} else {
				loadError = err instanceof ApiError ? err.message : 'Could not load your circles.';
			}
		} finally {
			loading = false;
		}
	});

	function people(n: number): string {
		return n === 1 ? 'person' : 'people';
	}

	// Match strength derived (not faked) from the cluster's mean similarity.
	function matchLabel(score: number): string {
		if (score >= 0.8) return 'Strong match';
		if (score >= 0.6) return 'Good match';
		return 'Emerging match';
	}
	function matchPct(score: number): number {
		return Math.max(0, Math.min(100, Math.round(score * 100)));
	}
</script>

<div class="page">
	<div class="head">
		<h1>My circles</h1>
		<p class="muted">
			Small groups of people taking steps toward goals like yours — grouped by what you're working
			on.
		</p>
	</div>

	{#if loading}
		<p class="muted state">Finding people on similar paths…</p>
	{:else if guest}
		<div class="card empty">
			<span class="tile tile-sky"><Icon name="lock" size={24} /></span>
			<h2>Sign in to find your circles</h2>
			<p class="muted">Circles are matched to your goals, so they're private to you.</p>
			<button class="btn btn-lime" onclick={() => void startLogin()}>Sign in</button>
		</div>
	{:else if loadError}
		<p class="error state">{loadError}</p>
	{:else if circles.length === 0}
		<div class="card empty">
			<span class="tile tile-sage"><Icon name="people" size={26} /></span>
			<h2>No circles yet</h2>
			<p class="muted">
				No circles yet — as more people set goals like yours, they'll show up here.
			</p>
		</div>
	{:else}
		<div class="grid">
			{#each circles as circle (circle.based_on_goal_id + circle.category)}
				{@const look = categoryLook(circle.category)}
				<article class="card circle">
					<div class="c-top">
						<span class="tile tile-{look.tile}"><Icon name={look.icon} size={22} /></span>
						<span class="pill">{matchLabel(circle.avg_score)}</span>
					</div>

					<h3>{look.category} circle</h3>
					<p class="count">
						<Icon name="people" size={15} />
						{circle.member_count}
						{people(circle.member_count)} working toward something similar
					</p>

					<div class="match">
						<div class="match-bar">
							<span style="width:{matchPct(circle.avg_score)}%"></span>
						</div>
						<span class="match-pct muted small">{matchPct(circle.avg_score)}% match</span>
					</div>

					<p class="based muted small">
						<Icon name="target" size={13} /> Based on your goal: {circle.based_on_goal_title}
					</p>

					<div class="c-foot">
						<button class="btn join" type="button" disabled title="Joining circles is coming soon">
							<Icon name="plus" size={15} /> Join circle
						</button>
						<span class="soon muted small">Joining coming soon</span>
					</div>
				</article>
			{/each}
		</div>
	{/if}
</div>

<style>
	.page {
		display: flex;
		flex-direction: column;
		gap: 22px;
	}
	.head h1 {
		font-size: 30px;
	}
	.head p {
		margin: 6px 0 0;
		font-size: 15px;
		max-width: 62ch;
	}
	.state {
		padding: 30px 0;
	}
	.error {
		color: #c0263a;
		font-size: 14px;
	}
	.empty {
		display: flex;
		flex-direction: column;
		align-items: center;
		text-align: center;
		gap: 12px;
		padding: 56px 24px;
		max-width: 480px;
		margin: 20px auto;
	}
	.empty h2 {
		font-size: 20px;
	}
	.empty p {
		margin: 0;
		max-width: 44ch;
	}
	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
		gap: 16px;
	}
	.circle {
		padding: 18px;
		display: flex;
		flex-direction: column;
		gap: 10px;
		min-width: 0;
	}
	.c-top {
		display: flex;
		align-items: center;
		justify-content: space-between;
	}
	.circle h3 {
		font-size: 18px;
		line-height: 1.25;
	}
	.count {
		display: flex;
		align-items: center;
		gap: 6px;
		font-size: 14px;
		margin: 0;
	}
	.match {
		display: flex;
		align-items: center;
		gap: 10px;
		margin-top: 2px;
	}
	.match-bar {
		flex: 1;
		height: 6px;
		border-radius: 999px;
		background: var(--track);
		overflow: hidden;
	}
	.match-bar span {
		display: block;
		height: 100%;
		border-radius: 999px;
		background: var(--lime);
	}
	.match-pct {
		white-space: nowrap;
	}
	.based {
		display: flex;
		align-items: center;
		gap: 6px;
		margin: 0;
	}
	.small {
		font-size: 12px;
	}
	.c-foot {
		display: flex;
		align-items: center;
		gap: 10px;
		border-top: 1px solid var(--line);
		padding-top: 12px;
		margin-top: 2px;
	}
	.join {
		opacity: 0.6;
		cursor: not-allowed;
	}
	.soon {
		white-space: nowrap;
	}
</style>
