<script lang="ts">
	import { createConversation, sendMessage, ApiError, type Message } from '$lib/api/client';

	let conversationId = $state<string | null>(null);
	let messages = $state<Message[]>([]);
	let draft = $state('');
	let sending = $state(false);
	let error = $state<string | null>(null);

	async function send(event: SubmitEvent) {
		event.preventDefault();
		error = null;

		const content = draft.trim();
		if (content === '' || sending) return;

		sending = true;
		// Optimistically show the user's message.
		const optimistic: Message = {
			id: `local-${Date.now()}`,
			conversation_id: conversationId ?? 'pending',
			role: 'user',
			content,
			created_at: new Date().toISOString()
		};
		messages = [...messages, optimistic];
		draft = '';

		try {
			// Lazily create a conversation on the first message.
			if (conversationId === null) {
				const convo = await createConversation();
				conversationId = convo.id;
			}

			// TODO: the backend streams this as SSE (token deltas); for now we take
			// the finished assistant message from a plain POST. See client.ts.
			const reply = await sendMessage(conversationId, content);
			messages = [...messages, reply];
		} catch (err) {
			error = err instanceof ApiError ? err.message : 'Failed to send message.';
		} finally {
			sending = false;
		}
	}
</script>

<div class="chat">
	<section class="roadmap-placeholder">
		<h2>Roadmap</h2>
		<p class="muted">
			Once you and AI Buddy settle on a goal and deadline, your roadmap (steps + due dates) will
			appear here. <em>Coming soon.</em>
		</p>
	</section>

	<section class="messages" aria-live="polite">
		{#if messages.length === 0}
			<p class="muted empty">
				Tell AI Buddy what you want to achieve — a goal, a deadline, where you're at right now — and
				it'll help you shape a plan.
			</p>
		{/if}
		{#each messages as message (message.id)}
			<div class="bubble {message.role}">
				<div class="content">{message.content}</div>
			</div>
		{/each}
		{#if sending}
			<div class="bubble assistant">
				<div class="content muted">Thinking…</div>
			</div>
		{/if}
	</section>

	{#if error}<p class="error">{error}</p>{/if}

	<form onsubmit={send}>
		<input type="text" placeholder="Message AI Buddy…" bind:value={draft} disabled={sending} />
		<button type="submit" disabled={sending || draft.trim() === ''}>Send</button>
	</form>
</div>

<style>
	.chat {
		display: flex;
		flex-direction: column;
		gap: 1rem;
	}

	.roadmap-placeholder {
		background: #eceafe;
		border: 1px dashed #b7aef7;
		border-radius: 12px;
		padding: 0.9rem 1rem;
	}

	.roadmap-placeholder h2 {
		margin: 0 0 0.3rem;
		font-size: 1rem;
		color: #4b3ff2;
	}

	.messages {
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
		min-height: 40vh;
		background: white;
		border: 1px solid #e2e4ee;
		border-radius: 14px;
		padding: 1rem;
	}

	.empty {
		margin: auto;
		text-align: center;
		max-width: 32ch;
	}

	.bubble {
		max-width: 80%;
		padding: 0.55rem 0.8rem;
		border-radius: 14px;
		font-size: 0.95rem;
		line-height: 1.4;
		white-space: pre-wrap;
		word-break: break-word;
	}

	.bubble.user {
		align-self: flex-end;
		background: #4b3ff2;
		color: white;
		border-bottom-right-radius: 4px;
	}

	.bubble.assistant,
	.bubble.system {
		align-self: flex-start;
		background: #f0f1f7;
		color: #1a1a2e;
		border-bottom-left-radius: 4px;
	}

	.muted {
		color: #6b6b85;
	}

	form {
		display: flex;
		gap: 0.5rem;
	}

	input {
		flex: 1;
		padding: 0.65rem 0.8rem;
		border: 1px solid #cfd2e0;
		border-radius: 10px;
		font-size: 1rem;
	}

	button {
		padding: 0.65rem 1.2rem;
		border: none;
		border-radius: 10px;
		background: #4b3ff2;
		color: white;
		font-size: 0.95rem;
		cursor: pointer;
	}

	button:disabled {
		opacity: 0.6;
		cursor: default;
	}

	.error {
		color: #c0263a;
		font-size: 0.9rem;
		margin: 0;
	}
</style>
