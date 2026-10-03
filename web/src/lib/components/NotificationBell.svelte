<script lang="ts">
	// Notifications bell + dropdown feed. Reads the shared notifications singleton
	// and renders coach reviews as small cards and other kinds as a generic line.
	// Guests (or an empty list) get an honest empty state. SSR is off app-wide, so
	// browser-only bits (document listeners) are still guarded to onMount.
	import { onMount, tick } from 'svelte';
	import { resolve } from '$app/paths';
	import Icon from './Icon.svelte';
	import { app } from '$lib/appState.svelte';
	import { notifications } from '$lib/notificationsState.svelte';
	import { relativeDate } from '$lib/format';

	let open = $state(false);
	let panelEl = $state<HTMLElement | null>(null);

	const unread = $derived(notifications.unread);

	// Shape of a coach notification's payload (see BACKEND CONTRACT / CoachReview).
	interface CoachPayload {
		status?: string;
		headline?: string;
		message?: string;
		questions?: string[];
		suggestions?: string[];
		risks?: string[];
		goal_title?: string;
	}

	// Status -> { label, class } for the coach pill. Text label always present, so
	// the state is never conveyed by color alone (a11y).
	function statusMeta(status: string | undefined): { label: string; cls: string } {
		switch (status) {
			case 'ahead':
				return { label: 'Ahead', cls: 'st-ahead' };
			case 'on_track':
				return { label: 'On track', cls: 'st-on_track' };
			case 'at_risk':
				return { label: 'At risk', cls: 'st-at_risk' };
			case 'off_track':
				return { label: 'Off track', cls: 'st-off_track' };
			default:
				return { label: 'Update', cls: 'st-on_track' };
		}
	}

	// A plain one-line summary for non-coach notifications, from whatever the
	// payload carries; falls back to a humanized kind so nothing renders blank.
	function genericLine(payload: Record<string, unknown>, kind: string): string {
		const pick = (k: string): string | null => {
			const v = payload[k];
			return typeof v === 'string' && v.trim() !== '' ? v : null;
		};
		return (
			pick('message') ??
			pick('headline') ??
			pick('title') ??
			pick('body') ??
			kind.replace(/[_-]+/g, ' ').replace(/^\w/, (c) => c.toUpperCase())
		);
	}

	async function toggle(): Promise<void> {
		open = !open;
		if (open) {
			// Opening the feed marks everything read (optimistic in the singleton).
			void notifications.markAllRead();
			await tick();
			panelEl?.focus();
		}
	}

	function close(): void {
		open = false;
	}

	onMount(() => {
		function onKey(e: KeyboardEvent): void {
			if (e.key === 'Escape' && open) close();
		}
		document.addEventListener('keydown', onKey);
		return () => document.removeEventListener('keydown', onKey);
	});
</script>

{#if !app.isGuest}
	<div class="bell-wrap">
		<button
			class="bellbtn"
			aria-label={unread > 0 ? `Notifications, ${unread} unread` : 'Notifications'}
			aria-haspopup="true"
			aria-expanded={open}
			onclick={toggle}
		>
			<Icon name="bell" size={19} />
			{#if unread > 0}
				<span class="count bell-count" aria-hidden="true">{unread > 99 ? '99+' : unread}</span>
			{/if}
		</button>

		{#if open}
			<!-- backdrop closes on outside click -->
			<button class="bell-backdrop" aria-label="Close notifications" onclick={close}></button>

			<div
				class="feed card"
				role="dialog"
				aria-label="Notifications"
				tabindex="-1"
				bind:this={panelEl}
			>
				<header class="feed-head">
					<h2>Notifications</h2>
					<button class="feed-close" aria-label="Close" onclick={close}>
						<Icon name="x" size={16} />
					</button>
				</header>

				{#if notifications.items.length === 0}
					<div class="feed-empty">
						<span class="tile tile-sage"><Icon name="bell" size={20} /></span>
						<p class="muted">You're all caught up.</p>
						<p class="muted small">Check-in reviews and reminders will show up here.</p>
					</div>
				{:else}
					<ul class="feed-list">
						{#each notifications.items as n (n.id)}
							<li class="feed-item" class:unread={n.read_at === null}>
								{#if n.kind === 'coach'}
									{@const p = n.payload as CoachPayload}
									{@const meta = statusMeta(p.status)}
									<div class="coach-card">
										<div class="coach-top">
											<span class="st-pill {meta.cls}">{meta.label}</span>
											<span class="muted small feed-time">{relativeDate(n.created_at)}</span>
										</div>
										{#if p.headline}<p class="coach-headline">{p.headline}</p>{/if}
										{#if p.message}<p class="coach-msg">{p.message}</p>{/if}
										{#if p.questions && p.questions.length > 0}
											<p class="coach-sub">Questions</p>
											<ul class="coach-sublist">
												{#each p.questions as q, i (i)}<li>{q}</li>{/each}
											</ul>
										{/if}
										{#if p.suggestions && p.suggestions.length > 0}
											<p class="coach-sub">Suggestions</p>
											<ul class="coach-sublist">
												{#each p.suggestions as s, i (i)}<li>{s}</li>{/each}
											</ul>
										{/if}
										{#if n.goal_id}
											<a
												class="feed-link"
												href={resolve('/goals/[id]', { id: n.goal_id })}
												onclick={close}
											>
												View goal <Icon name="chevron-r" size={13} />
											</a>
										{/if}
									</div>
								{:else}
									<div class="generic">
										<p class="generic-line">{genericLine(n.payload, n.kind)}</p>
										<div class="coach-top">
											<span class="muted small feed-time">{relativeDate(n.created_at)}</span>
											{#if n.goal_id}
												<a
													class="feed-link"
													href={resolve('/goals/[id]', { id: n.goal_id })}
													onclick={close}
												>
													View goal <Icon name="chevron-r" size={13} />
												</a>
											{/if}
										</div>
									</div>
								{/if}
							</li>
						{/each}
					</ul>
				{/if}
			</div>
		{/if}
	</div>
{/if}

<style>
	.bell-wrap {
		position: relative;
		display: inline-flex;
	}
	.bellbtn {
		position: relative;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 38px;
		height: 38px;
		border-radius: 10px;
		border: 1px solid var(--line);
		background: var(--card);
		color: var(--ink);
	}
	.bellbtn:hover {
		background: var(--tag-bg);
	}
	/* reuse the sidebar .count pill look */
	.bell-count {
		position: absolute;
		top: -6px;
		right: -6px;
		margin: 0;
		background: var(--teal);
		color: #fff;
		font-size: 10px;
		font-weight: 700;
		min-width: 18px;
		height: 18px;
		border-radius: 999px;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		padding: 0 5px;
		border: 2px solid var(--bg);
	}

	.bell-backdrop {
		position: fixed;
		inset: 0;
		z-index: 40;
		border: none;
		background: transparent;
	}

	.feed {
		position: absolute;
		top: calc(100% + 10px);
		right: 0;
		z-index: 50;
		width: min(360px, calc(100vw - 32px));
		max-height: min(70vh, 560px);
		overflow-y: auto;
		padding: 0;
	}
	.feed-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 14px 16px 10px;
		position: sticky;
		top: 0;
		background: var(--card);
		border-bottom: 1px solid var(--line);
	}
	.feed-head h2 {
		font-size: 15px;
	}
	.feed-close {
		background: transparent;
		border: none;
		color: var(--muted);
		display: inline-flex;
		padding: 4px;
		border-radius: 8px;
	}
	.feed-close:hover {
		color: var(--ink);
		background: var(--tag-bg);
	}

	.feed-empty {
		display: flex;
		flex-direction: column;
		align-items: center;
		text-align: center;
		gap: 6px;
		padding: 30px 20px;
	}
	.feed-empty p {
		margin: 0;
	}
	.small {
		font-size: 12px;
	}

	.feed-list {
		list-style: none;
		margin: 0;
		padding: 6px;
		display: flex;
		flex-direction: column;
		gap: 4px;
	}
	.feed-item {
		border-radius: 12px;
		padding: 10px 12px;
	}
	.feed-item.unread {
		background: var(--tag-bg);
	}

	.coach-top {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 8px;
		flex-wrap: wrap;
	}
	.feed-time {
		font-size: 11px;
	}

	.st-pill {
		display: inline-block;
		border-radius: 999px;
		padding: 2px 10px;
		font-size: 11px;
		font-weight: 700;
	}
	.st-on_track {
		background: color-mix(in srgb, var(--sage) 55%, var(--card));
		color: var(--sage-ink);
	}
	.st-ahead {
		background: color-mix(in srgb, var(--lime) 45%, var(--card));
		color: var(--ink);
	}
	.st-at_risk {
		background: #fbe7c4;
		color: #9a6b19;
	}
	.st-off_track {
		background: #f7d6d2;
		color: #9f3a2e;
	}

	.coach-headline {
		margin: 8px 0 2px;
		font-size: 14px;
		font-weight: 700;
		line-height: 1.3;
	}
	.coach-msg {
		margin: 2px 0 0;
		font-size: 13px;
		line-height: 1.45;
	}
	.coach-sub {
		margin: 8px 0 2px;
		font-size: 11px;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--muted);
	}
	.coach-sublist {
		margin: 0;
		padding-left: 18px;
		font-size: 13px;
		line-height: 1.45;
	}
	.coach-sublist li {
		margin: 1px 0;
	}

	.generic-line {
		margin: 0 0 6px;
		font-size: 14px;
		line-height: 1.4;
	}

	.feed-link {
		display: inline-flex;
		align-items: center;
		gap: 2px;
		margin-top: 8px;
		font-size: 13px;
		font-weight: 600;
		color: var(--sky-ink);
	}
	.feed-link:hover {
		text-decoration: underline;
	}

	@media (max-width: 480px) {
		.feed {
			position: fixed;
			top: auto;
			right: 8px;
			left: 8px;
			width: auto;
			max-height: 72vh;
		}
	}
</style>
