<script lang="ts">
	import '../app.css';
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { resolve } from '$app/paths';
	import Icon from '$lib/components/Icon.svelte';
	import NotificationBell from '$lib/components/NotificationBell.svelte';
	import { app } from '$lib/appState.svelte';
	import { notifications } from '$lib/notificationsState.svelte';
	import { coach } from '$lib/coachState.svelte';
	import { IS_APP } from '$lib/api/deviceToken';
	import { startLogin } from '$lib/app/login';

	let { children } = $props();

	let menuOpen = $state(false);

	onMount(() => {
		let unlistenDeepLink: (() => void) | undefined;

		void (async () => {
			// App build: the OIDC login returns the device token via the
			// `dev.aibuddy.app://auth?token=<raw>` deep link. Register the handler
			// (and process any cold-start launch URL) before the initial load so a
			// launch via the login deep link lands signed in. When a token arrives,
			// re-fetch /auth/me + goals so the guest state flips to signed-in.
			if (IS_APP) {
				const { initDeepLinkAuth } = await import('$lib/app/deepLinkAuth');
				unlistenDeepLink = await initDeepLinkAuth(() => app.reload());
			}
			await app.ensureLoaded();
			// Hydrate the notifications bell once auth is known (client-side only;
			// onMount never runs on the server). Guests resolve to an empty list.
			if (!app.isGuest) void notifications.refresh();
		})();

		return () => unlistenDeepLink?.();
	});

	const nav = [
		{ href: '/', label: 'Today', icon: 'home', badge: false },
		{ href: '/goals', label: 'My goals', icon: 'target', badge: true },
		{ href: '/discover', label: 'Discover', icon: 'compass', badge: false },
		{ href: '/circles', label: 'My circles', icon: 'people', badge: false }
	] as const;

	const path = $derived(page.url.pathname);
	const home = resolve('/');
	const settingsPath = resolve('/settings');

	// `target` is an already-resolved path (from resolve()).
	function isActive(target: string): boolean {
		if (target === home) return path === home;
		return path === target || path.startsWith(`${target}/`);
	}

	// Section label for the top bar, derived from the active route.
	const sectionLabel = $derived.by(() => {
		if (path.startsWith(resolve('/goals'))) return 'My goals';
		if (path.startsWith(resolve('/chat'))) return 'Goal coach';
		if (path.startsWith(resolve('/wallet'))) return 'Your wallet';
		if (path.startsWith(resolve('/discover'))) return 'Discover';
		if (path.startsWith(resolve('/circles'))) return 'My circles';
		if (path.startsWith(settingsPath)) return 'Settings';
		return 'Today';
	});

	// Open the coach on a brand-new conversation, from any "New goal" surface.
	// Guests are sent to login first — the coach flow requires auth (would 401).
	async function startCoach(): Promise<void> {
		await app.ensureLoaded();
		if (app.isGuest) {
			await startLogin();
			return;
		}
		coach.startNew();
		await goto(resolve('/chat'));
	}

	const goalCount = $derived(app.goals.length);
	const initial = $derived(
		(app.me?.display_name ?? app.me?.email ?? 'G').trim().charAt(0).toUpperCase()
	);
</script>

<div class="shell">
	<!-- backdrop for mobile drawer -->
	{#if menuOpen}
		<button class="backdrop" aria-label="Close menu" onclick={() => (menuOpen = false)}></button>
	{/if}

	<aside class="sidebar" class:open={menuOpen}>
		<a class="brand" href={home} onclick={() => (menuOpen = false)}>
			<span class="badge"><Icon name="check" size={18} stroke={2.4} /></span>
			<span class="wordmark">buddy<span class="dot-lime">.</span></span>
		</a>
		<p class="tagline eyebrow">Your everyday, better</p>

		<nav>
			{#each nav as item (item.href)}
				<a
					class="navitem"
					class:active={isActive(resolve(item.href))}
					href={resolve(item.href)}
					onclick={() => (menuOpen = false)}
				>
					<Icon name={item.icon} size={19} />
					<span>{item.label}</span>
					{#if item.badge && goalCount > 0}
						<span class="count">{goalCount}</span>
					{/if}
				</a>
			{/each}
		</nav>

		<div class="promo">
			<p class="promo-head"><Icon name="sparkle" size={16} /> A little closer. Every single day.</p>
			<p class="muted small">You don't need a new you.</p>
			<a class="plus-card" href={resolve('/wallet')} onclick={() => (menuOpen = false)}>
				<span>
					<strong>Meet Buddy Plus</strong>
					<span class="muted small">More room for your ambitions</span>
				</span>
				<Icon name="chevron-r" size={16} />
			</a>
		</div>

		<div class="userrow">
			{#if app.me}
				<span class="avatar">{initial}</span>
				<span class="who">
					<strong>{app.me.display_name ?? app.me.email ?? 'You'}</strong>
					{#if app.me.display_name && app.me.email}
						<span class="muted small">{app.me.email}</span>
					{/if}
				</span>
			{:else}
				<span class="avatar">G</span>
				<span class="who">
					<strong>Guest</strong>
					<button class="signin small" onclick={() => void startLogin()}>Sign in</button>
				</span>
			{/if}
			<a
				class="gearbtn"
				class:active={isActive(settingsPath)}
				href={settingsPath}
				aria-label="Settings"
				onclick={() => (menuOpen = false)}
			>
				<Icon name="gear" size={18} />
			</a>
		</div>
	</aside>

	<div class="main">
		<header class="topbar">
			<div class="topbar-left">
				<button class="menubtn" aria-label="Open menu" onclick={() => (menuOpen = true)}>
					<Icon name="menu" size={20} />
				</button>
				<span class="section">{sectionLabel}</span>
			</div>
			<div class="topbar-right">
				<span class="private muted"><Icon name="lock" size={15} /> Your private space</span>
				<NotificationBell />
				<button class="btn newgoal" onclick={startCoach}>
					<Icon name="plus" size={16} /> New goal
				</button>
			</div>
		</header>

		<div class="content">
			{@render children()}
		</div>
	</div>
</div>

<style>
	.shell {
		display: flex;
		min-height: 100vh;
		align-items: stretch;
	}

	/* ---- sidebar ---- */
	.sidebar {
		width: 248px;
		flex: none;
		background: var(--bg);
		border-right: 1px solid var(--line);
		padding: calc(22px + var(--safe-top)) 16px calc(22px + var(--safe-bottom));
		padding-left: calc(16px + var(--safe-left));
		display: flex;
		flex-direction: column;
		gap: 18px;
		position: sticky;
		top: 0;
		height: 100vh;
		overflow-y: auto;
	}
	.brand {
		display: flex;
		align-items: center;
		gap: 10px;
	}
	.badge {
		width: 34px;
		height: 34px;
		border-radius: 10px;
		background: var(--teal);
		color: var(--lime);
		display: inline-flex;
		align-items: center;
		justify-content: center;
		flex: none;
	}
	.wordmark {
		font-weight: 700;
		font-size: 18px;
		letter-spacing: -0.02em;
	}
	.tagline {
		margin: -8px 0 4px;
	}

	nav {
		display: flex;
		flex-direction: column;
		gap: 2px;
	}
	.navitem {
		display: flex;
		align-items: center;
		gap: 11px;
		padding: 10px 12px;
		border-radius: 12px;
		color: var(--muted);
		font-weight: 600;
		font-size: 14px;
	}
	.navitem:hover {
		color: var(--ink);
	}
	.navitem.active {
		background: rgba(200, 220, 170, 0.35);
		color: var(--ink);
	}
	.count {
		margin-left: auto;
		background: var(--teal);
		color: #fff;
		font-size: 11px;
		font-weight: 700;
		min-width: 20px;
		height: 20px;
		border-radius: 999px;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		padding: 0 6px;
	}

	.promo {
		margin-top: auto;
		display: flex;
		flex-direction: column;
		gap: 8px;
	}
	.promo-head {
		display: flex;
		align-items: center;
		gap: 7px;
		font-weight: 700;
		font-size: 14px;
		margin: 0;
		line-height: 1.3;
	}
	.small {
		font-size: 12px;
	}
	.plus-card {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 8px;
		background: var(--card);
		border: 1px solid var(--line);
		border-radius: 12px;
		padding: 11px 12px;
		box-shadow: var(--shadow);
	}
	.plus-card strong {
		display: block;
		font-size: 13px;
	}

	.userrow {
		display: flex;
		align-items: center;
		gap: 10px;
		border-top: 1px solid var(--line);
		padding-top: 16px;
		color: var(--muted);
	}
	.avatar {
		width: 36px;
		height: 36px;
		border-radius: 999px;
		background: var(--sage);
		color: var(--sage-ink);
		display: inline-flex;
		align-items: center;
		justify-content: center;
		font-weight: 700;
		flex: none;
	}
	.who {
		display: flex;
		flex-direction: column;
		line-height: 1.2;
		flex: 1;
		min-width: 0;
	}
	.who strong {
		color: var(--ink);
		font-size: 13px;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.signin {
		color: var(--sky-ink);
		font-weight: 600;
		background: none;
		border: none;
		padding: 0;
		text-align: left;
		align-self: flex-start;
	}
	.signin:hover {
		text-decoration: underline;
	}
	.gearbtn {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 34px;
		height: 34px;
		border-radius: 10px;
		color: var(--muted);
		flex: none;
	}
	.gearbtn:hover {
		color: var(--ink);
		background: var(--tag-bg);
	}
	.gearbtn.active {
		background: rgba(200, 220, 170, 0.35);
		color: var(--ink);
	}

	/* ---- main ---- */
	.main {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
	}
	.topbar {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
		/* Add the status-bar inset on top so the bar's content clears the Android
		   status bar (the sticky, translucent bar itself extends up under it). */
		padding: calc(18px + var(--safe-top)) calc(28px + var(--safe-right)) 18px
			calc(28px + var(--safe-left));
		border-bottom: 1px solid var(--line);
		position: sticky;
		top: 0;
		background: color-mix(in srgb, var(--bg) 88%, transparent);
		backdrop-filter: blur(6px);
		/* Above page content and tappable; the drawer + its backdrop (below) sit
		   above this only while the menu is open. */
		z-index: 20;
	}
	.topbar-left,
	.topbar-right {
		display: flex;
		align-items: center;
		gap: 14px;
	}
	.section {
		font-weight: 700;
		font-size: 16px;
	}
	.private {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		font-size: 13px;
	}
	.newgoal {
		padding: 9px 14px;
		border-radius: 9px;
	}
	.menubtn {
		display: none;
		background: transparent;
		border: 1px solid var(--line);
		border-radius: 9px;
		padding: 6px;
		color: var(--ink);
	}

	.content {
		padding: 24px calc(28px + var(--safe-right)) calc(60px + var(--safe-bottom))
			calc(28px + var(--safe-left));
		max-width: 1180px;
		width: 100%;
	}

	.backdrop {
		display: none;
	}

	/* ---- responsive ---- */
	@media (max-width: 900px) {
		.sidebar {
			position: fixed;
			left: 0;
			top: 0;
			width: min(288px, 84vw);
			/* Above the topbar (20) and its backdrop (30) so the open drawer sits on
			   top; when closed it's translated fully off-canvas and can't intercept
			   taps on the topbar. */
			z-index: 40;
			transform: translateX(-100%);
			transition: transform 0.2s ease;
			box-shadow: 0 8px 40px rgba(20, 40, 45, 0.18);
		}
		.sidebar.open {
			transform: translateX(0);
		}
		.menubtn {
			display: inline-flex;
			align-items: center;
			justify-content: center;
			min-width: 44px;
			min-height: 44px;
		}
		.newgoal {
			padding: 11px 15px;
		}
		.backdrop {
			display: block;
			position: fixed;
			inset: 0;
			/* Between the topbar (20) and the drawer (40): dims + blocks the topbar's
			   buttons while the menu is open, but stays below the drawer itself. */
			z-index: 30;
			border: none;
			background: rgba(20, 40, 45, 0.35);
		}
		.topbar {
			padding: calc(14px + var(--safe-top)) calc(16px + var(--safe-right)) 14px
				calc(16px + var(--safe-left));
		}
		.content {
			padding: 18px calc(16px + var(--safe-right)) calc(56px + var(--safe-bottom))
				calc(16px + var(--safe-left));
		}
		.private {
			display: none;
		}
	}
</style>
