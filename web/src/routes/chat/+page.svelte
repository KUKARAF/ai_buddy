<script lang="ts">
	import { onMount } from 'svelte';
	import { resolve } from '$app/paths';
	import Icon from '$lib/components/Icon.svelte';
	import { createConversation, streamMessage, getSettings, ApiError } from '$lib/api/client';
	import { app } from '$lib/appState.svelte';

	// The active coach model, shown as a small muted label in the header.
	// Best-effort: a failure (guest/offline) just hides the label.
	let activeModel = $state<string | null>(null);
	onMount(async () => {
		try {
			const settings = await getSettings();
			const match = settings.allowed_models.find((m) => m.id === settings.chat_model);
			activeModel = match?.label ?? settings.chat_model;
		} catch {
			activeModel = null;
		}
	});

	interface ChatMessage {
		id: string;
		role: 'user' | 'assistant';
		content: string;
	}

	let conversationId = $state<string | null>(null);
	let messages = $state<ChatMessage[]>([]);
	let draft = $state('');
	let sending = $state(false);
	let error = $state<string | null>(null);
	let needsTopup = $state(false);

	async function send(event: SubmitEvent) {
		event.preventDefault();
		error = null;
		needsTopup = false;

		const content = draft.trim();
		if (content === '' || sending) return;

		sending = true;
		messages = [...messages, { id: `u-${Date.now()}`, role: 'user', content }];
		draft = '';

		const assistantId = `a-${Date.now()}`;
		let assistantAdded = false;
		const appendToken = (token: string) => {
			if (!assistantAdded) {
				messages = [...messages, { id: assistantId, role: 'assistant', content: token }];
				assistantAdded = true;
			} else {
				messages = messages.map((m) =>
					m.id === assistantId ? { ...m, content: m.content + token } : m
				);
			}
		};

		try {
			if (conversationId === null) {
				const convo = await createConversation();
				conversationId = convo.id;
			}

			await streamMessage(conversationId, content, {
				onToken: appendToken,
				onError: (message) => {
					error = message || 'The coach hit a snag. Please try again.';
				}
			});
		} catch (err) {
			if (err instanceof ApiError && err.status === 402) {
				needsTopup = true;
			} else {
				error = err instanceof ApiError ? err.message : 'Failed to send message.';
			}
		} finally {
			sending = false;
		}
	}
</script>

<div class="coach">
	<header class="c-head">
		<span class="badge"><Icon name="check" size={18} stroke={2.4} /></span>
		<div>
			<h1>Let's shape your goal</h1>
			<p class="muted">
				Tell the coach what you want — it'll help you turn it into a small, doable plan.
			</p>
			{#if activeModel}
				<a class="model-tag muted" href={resolve('/settings')}>
					<Icon name="gear" size={13} />
					{activeModel}
				</a>
			{/if}
		</div>
	</header>

	<section class="thread" aria-live="polite">
		{#if messages.length === 0}
			<div class="empty">
				<span class="tile tile-sage"><Icon name="sparkle" size={22} /></span>
				<p>
					Hi {app.firstName}. What's something you'd love to be a little closer to? A goal, a
					deadline, or just where you're stuck right now — start anywhere.
				</p>
			</div>
		{/if}

		{#each messages as message (message.id)}
			{#if message.role === 'assistant'}
				<div class="row assistant">
					<span class="av"><Icon name="check" size={14} stroke={2.6} /></span>
					<div class="bubble a-bubble">{message.content}</div>
				</div>
			{:else}
				<div class="row user">
					<div class="bubble u-bubble">{message.content}</div>
				</div>
			{/if}
		{/each}

		{#if sending && messages[messages.length - 1]?.role === 'user'}
			<div class="row assistant">
				<span class="av"><Icon name="check" size={14} stroke={2.6} /></span>
				<div class="bubble a-bubble muted">Thinking…</div>
			</div>
		{/if}
	</section>

	{#if needsTopup}
		<div class="notice">
			<Icon name="wallet" size={18} />
			<span>Add a little to your wallet to keep going.</span>
			<a class="btn btn-lime" href={resolve('/wallet')}>Open wallet</a>
		</div>
	{/if}
	{#if error}<p class="error">{error}</p>{/if}

	<form onsubmit={send}>
		<input type="text" placeholder="Message your coach…" bind:value={draft} disabled={sending} />
		<button
			type="submit"
			class="sendbtn"
			disabled={sending || draft.trim() === ''}
			aria-label="Send"
		>
			<Icon name="send" size={18} />
		</button>
	</form>
</div>

<style>
	.coach {
		max-width: 720px;
		margin: 0 auto;
		display: flex;
		flex-direction: column;
		gap: 18px;
		min-height: calc(100vh - 180px);
	}
	.c-head {
		display: flex;
		align-items: center;
		gap: 12px;
	}
	.c-head h1 {
		font-size: 24px;
	}
	.c-head p {
		margin: 2px 0 0;
		font-size: 14px;
	}
	.model-tag {
		display: inline-flex;
		align-items: center;
		gap: 5px;
		margin-top: 6px;
		font-size: 12px;
		font-weight: 500;
	}
	.model-tag:hover {
		color: var(--ink);
	}
	.badge {
		width: 40px;
		height: 40px;
		border-radius: 12px;
		background: var(--teal);
		color: var(--lime);
		display: inline-flex;
		align-items: center;
		justify-content: center;
		flex: none;
	}

	.thread {
		flex: 1;
		display: flex;
		flex-direction: column;
		gap: 14px;
		padding: 4px 0;
	}
	.empty {
		display: flex;
		flex-direction: column;
		align-items: center;
		text-align: center;
		gap: 12px;
		color: var(--muted);
		margin: auto;
		max-width: 42ch;
		padding: 30px 0;
	}

	.row {
		display: flex;
		gap: 10px;
		align-items: flex-end;
	}
	.row.user {
		justify-content: flex-end;
	}
	.av {
		width: 28px;
		height: 28px;
		border-radius: 999px;
		background: var(--lime);
		color: var(--ink);
		display: inline-flex;
		align-items: center;
		justify-content: center;
		flex: none;
	}
	.bubble {
		max-width: 78%;
		padding: 12px 15px;
		font-size: 15px;
		line-height: 1.5;
		white-space: pre-wrap;
		word-break: break-word;
	}
	.a-bubble {
		background: var(--card);
		border: 1px solid var(--line);
		border-radius: 4px 16px 16px 16px;
		box-shadow: var(--shadow);
	}
	.u-bubble {
		background: var(--teal);
		color: #fff;
		border-radius: 16px 16px 4px 16px;
	}

	.notice {
		display: flex;
		align-items: center;
		gap: 12px;
		background: color-mix(in srgb, var(--lime) 22%, var(--card));
		border: 1px solid var(--line);
		border-radius: 12px;
		padding: 12px 14px;
		font-size: 14px;
		font-weight: 500;
	}
	.notice .btn {
		margin-left: auto;
	}
	.error {
		color: #c0263a;
		font-size: 14px;
		margin: 0;
	}

	form {
		display: flex;
		gap: 10px;
		position: sticky;
		bottom: 12px;
		background: var(--card);
		border: 1px solid var(--line);
		border-radius: 14px;
		padding: 8px 8px 8px 16px;
		box-shadow: var(--shadow);
	}
	input {
		flex: 1;
		border: none;
		outline: none;
		background: transparent;
		font-size: 15px;
		color: var(--ink);
		font-family: inherit;
	}
	.sendbtn {
		border: none;
		border-radius: 10px;
		background: var(--lime);
		color: var(--ink);
		width: 42px;
		height: 42px;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		flex: none;
	}
</style>
