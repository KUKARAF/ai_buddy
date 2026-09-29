<script lang="ts">
	import Icon from './Icon.svelte';
	interface Props {
		title: string;
		icon: string;
		tile: 'peach' | 'lavender' | 'sage' | 'sky';
		blurb: string;
	}
	let { title, icon, tile, blurb }: Props = $props();
</script>

<div class="page">
	<h1>{title}</h1>
	<div class="card empty">
		<span class="tile tile-{tile}"><Icon name={icon} size={26} /></span>
		<h2>Coming soon</h2>
		<p class="muted">{blurb}</p>
		<span class="pill">In the works</span>
	</div>
</div>

<style>
	.page {
		display: flex;
		flex-direction: column;
		gap: 22px;
	}
	.page h1 {
		font-size: 30px;
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
		max-width: 42ch;
	}
</style>
