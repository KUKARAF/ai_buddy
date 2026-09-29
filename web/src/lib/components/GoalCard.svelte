<script lang="ts">
	import { resolve } from '$app/paths';
	import Icon from './Icon.svelte';

	interface Props {
		title: string;
		category: string;
		tile: 'peach' | 'lavender' | 'sage' | 'sky';
		icon: string;
		/** Short deadline like "12 Dec", or null when none is set. */
		deadline: string | null;
		done: number;
		total: number;
		href?: string;
	}
	let {
		title,
		category,
		tile,
		icon,
		deadline,
		done,
		total,
		href = resolve('/goals')
	}: Props = $props();

	const pct = $derived(total > 0 ? Math.round((done / total) * 100) : 0);
</script>

<article class="card goal">
	<div class="top">
		<span class="tile tile-{tile}"><Icon name={icon} size={22} /></span>
		<span class="pill">{category}</span>
	</div>

	<h3 class="title">{title}</h3>

	<p class="finish muted">
		<Icon name="calendar" size={14} />
		{#if deadline}{deadline} finish line{:else}No finish line yet{/if}
	</p>

	<div class="milestones">
		<span>{done} of {total} milestones</span>
		<span class="pct">{pct}%</span>
	</div>
	<div class="progress"><span style="width:{pct}%"></span></div>

	<div class="foot">
		<span class="priv"><Icon name="lock" size={14} /> Private goal</span>
		<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- href is always a resolve()'d path from the caller/default -->
		<a class="viewplan" {href}>View plan <Icon name="chevron-r" size={14} /></a>
	</div>
</article>

<style>
	.goal {
		padding: 18px;
		display: flex;
		flex-direction: column;
		gap: 12px;
		min-width: 0;
	}
	.top {
		display: flex;
		align-items: center;
		justify-content: space-between;
	}
	.title {
		font-size: 18px;
		font-weight: 700;
		line-height: 1.25;
		display: -webkit-box;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
		overflow: hidden;
		min-height: 2.5em;
	}
	.finish {
		display: flex;
		align-items: center;
		gap: 6px;
		font-size: 13px;
		margin: 0;
	}
	.milestones {
		display: flex;
		justify-content: space-between;
		align-items: baseline;
		font-size: 13px;
		color: var(--muted);
		margin-top: 2px;
	}
	.pct {
		font-weight: 700;
		color: var(--ink);
	}
	.foot {
		display: flex;
		align-items: center;
		justify-content: space-between;
		border-top: 1px solid var(--line);
		padding-top: 12px;
		margin-top: 2px;
		font-size: 13px;
	}
	.priv {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		color: var(--muted);
	}
	.viewplan {
		display: inline-flex;
		align-items: center;
		gap: 3px;
		font-weight: 600;
		color: var(--ink);
	}
</style>
