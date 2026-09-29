<script lang="ts">
	import { page } from '$app/state';
	import { resolve } from '$app/paths';

	let { children } = $props();

	const nav = [
		{ href: resolve('/'), label: 'Wallet' },
		{ href: resolve('/chat'), label: 'Chat' }
	];
</script>

<div class="app">
	<header>
		<h1>AI Buddy</h1>
		<nav>
			{#each nav as item (item.href)}
				<a href={item.href} class:active={page.url.pathname === item.href}>
					{item.label}
				</a>
			{/each}
		</nav>
	</header>
	<main>
		{@render children()}
	</main>
</div>

<style>
	:global(body) {
		margin: 0;
		font-family:
			system-ui,
			-apple-system,
			Segoe UI,
			Roboto,
			sans-serif;
		color: #1a1a2e;
		background: #f6f7fb;
	}

	.app {
		max-width: 720px;
		margin: 0 auto;
		padding: 0 1rem 3rem;
	}

	header {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 1rem;
		padding: 1.25rem 0;
		border-bottom: 1px solid #e2e4ee;
		margin-bottom: 1.5rem;
		flex-wrap: wrap;
	}

	h1 {
		margin: 0;
		font-size: 1.4rem;
		letter-spacing: -0.02em;
	}

	nav {
		display: flex;
		gap: 0.5rem;
	}

	nav a {
		padding: 0.4rem 0.85rem;
		border-radius: 999px;
		text-decoration: none;
		color: #4a4a6a;
		font-size: 0.95rem;
	}

	nav a.active {
		background: #4b3ff2;
		color: white;
	}
</style>
