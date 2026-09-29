<script lang="ts">
	import { onMount, tick } from 'svelte';
	import { resolve } from '$app/paths';
	import Icon from '$lib/components/Icon.svelte';
	import {
		createConversation,
		getConversation,
		sendAgentMessage,
		getSettings,
		ApiError
	} from '$lib/api/client';
	import { app } from '$lib/appState.svelte';
	import { startLogin } from '$lib/app/login';
	import { coach } from '$lib/coachState.svelte';

	// The active coach model, shown as a small muted label in the header.
	// Best-effort: a failure (guest/offline) just hides the label.
	let activeModel = $state<string | null>(null);
	// Only render the coach once auth is confirmed; guests are redirected to login.
	let ready = $state(false);

	interface ChatMessage {
		id: string;
		role: 'user' | 'assistant';
		content: string;
		/** Set on an assistant message when the coach created a goal that turn. */
		createdGoalId?: string | null;
	}

	let conversationId = $state<string | null>(null);
	let messages = $state<ChatMessage[]>([]);
	let draft = $state('');
	let sending = $state(false);
	let error = $state<string | null>(null);
	let needsTopup = $state(false);
	let unavailable = $state(false);
	let thread = $state<HTMLElement | null>(null);

	function opening(): ChatMessage {
		return {
			id: 'coach-opening',
			role: 'assistant',
			content: `Hey ${app.firstName} — what's something you'd love to make happen? Tell me about it, however rambly. I'll help you shape it into a real, doable goal.`
		};
	}

	async function scrollToEnd(): Promise<void> {
		await tick();
		if (thread) thread.scrollTop = thread.scrollHeight;
	}

	onMount(() => {
		void (async () => {
			// The coach requires auth; a guest here would only hit 401s. Redirect to
			// login before rendering the composer or creating any conversation.
			await app.ensureLoaded();
			if (app.isGuest) {
				await startLogin();
				return;
			}
			ready = true;

			// Best-effort model label (independent of the coach flow).
			void (async () => {
				try {
					const settings = await getSettings();
					const match = settings.allowed_models.find((m) => m.id === settings.chat_model);
					activeModel = match?.label ?? settings.chat_model;
				} catch {
					activeModel = null;
				}
			})();

			const seed = coach.takeSeed();
			if (coach.conversationId !== null && seed === null) {
				// Resume an in-session conversation: load its history.
				conversationId = coach.conversationId;
				try {
					const detail = await getConversation(conversationId);
					messages = detail.messages
						.filter((m) => m.role !== 'system')
						.map((m) => ({
							id: m.id,
							role: m.role === 'user' ? 'user' : 'assistant',
							content: m.content
						}));
				} catch {
					/* fall back to a fresh opening if history can't be loaded */
				}
				if (messages.length === 0) messages = [opening()];
			} else {
				// Fresh start (default, and whenever an entry point requested one).
				conversationId = null;
				messages = [opening()];
				if (seed !== null && seed.trim() !== '') {
					await sendContent(seed.trim());
				}
			}
			void scrollToEnd();
		})();
	});

	async function sendContent(content: string): Promise<void> {
		if (content === '' || sending) return;

		error = null;
		needsTopup = false;
		unavailable = false;
		sending = true;
		messages = [...messages, { id: `u-${Date.now()}`, role: 'user', content }];
		void scrollToEnd();

		try {
			if (conversationId === null) {
				const convo = await createConversation();
				conversationId = convo.id;
				coach.conversationId = convo.id;
			}

			const res = await sendAgentMessage(conversationId, content);
			messages = [
				...messages,
				{
					id: `a-${Date.now()}`,
					role: 'assistant',
					content: res.reply,
					createdGoalId: res.created_goal_id
				}
			];
			if (res.created_goal_id) void app.refreshGoals();
		} catch (err) {
			if (err instanceof ApiError && err.status === 402) {
				needsTopup = true;
			} else if (err instanceof ApiError && err.status === 400) {
				unavailable = true;
			} else {
				error = err instanceof ApiError ? err.message : 'Failed to reach your coach.';
			}
		} finally {
			sending = false;
			void scrollToEnd();
		}
	}

	async function send(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		const content = draft.trim();
		if (content === '' || sending) return;
		draft = '';
		await sendContent(content);
	}
</script>

{#if !ready}
	<div class="loading-gate muted">Getting your coach ready…</div>
{:else}
	<div class="coach">
		<header class="c-head">
			<span class="badge"><Icon name="check" size={18} stroke={2.4} /></span>
			<div>
				<h1>Let's shape your goal</h1>
				<p class="muted">
					Talk it through with your coach — it'll turn your someday into a small, doable plan.
				</p>
				{#if activeModel}
					<a class="model-tag muted" href={resolve('/settings')}>
						<Icon name="gear" size={13} />
						{activeModel}
					</a>
				{/if}
			</div>
		</header>

		<section class="thread" aria-live="polite" bind:this={thread}>
			{#each messages as message (message.id)}
				{#if message.role === 'assistant'}
					<div class="row assistant">
						<span class="av"><Icon name="check" size={14} stroke={2.6} /></span>
						<div class="a-col">
							<div class="bubble a-bubble">{message.content}</div>
							{#if message.createdGoalId}
								<div class="goal-created">
									<span class="gc-check"><Icon name="check" size={14} stroke={2.6} /></span>
									<span class="gc-text">Goal created</span>
									<a
										class="btn btn-lime gc-btn"
										href={resolve('/goals/[id]', { id: message.createdGoalId })}
									>
										View your plan <Icon name="chevron-r" size={15} />
									</a>
								</div>
							{/if}
						</div>
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
					<div class="bubble a-bubble muted">Your coach is thinking…</div>
				</div>
			{/if}
		</section>

		{#if needsTopup}
			<div class="notice">
				<Icon name="wallet" size={18} />
				<span>Your wallet's empty — add a little to keep talking with your coach.</span>
				<a class="btn btn-lime" href={resolve('/wallet')}>Open wallet</a>
			</div>
		{/if}
		{#if unavailable}
			<div class="notice">
				<Icon name="sparkle" size={18} />
				<span>Your coach isn't available right now. Please try again a little later.</span>
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
{/if}

<style>
	.loading-gate {
		max-width: 720px;
		margin: 0 auto;
		padding: 60px 0;
		text-align: center;
		font-size: 15px;
	}
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

	.row {
		display: flex;
		gap: 10px;
		align-items: flex-end;
	}
	.row.user {
		justify-content: flex-end;
	}
	.a-col {
		display: flex;
		flex-direction: column;
		gap: 8px;
		max-width: 78%;
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
	.a-col .bubble {
		max-width: 100%;
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

	.goal-created {
		display: flex;
		align-items: center;
		gap: 10px;
		background: color-mix(in srgb, var(--lime) 22%, var(--card));
		border: 1px solid var(--line);
		border-radius: 12px;
		padding: 10px 12px;
		font-size: 14px;
		font-weight: 600;
	}
	.gc-check {
		width: 24px;
		height: 24px;
		border-radius: 999px;
		background: var(--lime);
		color: var(--ink);
		display: inline-flex;
		align-items: center;
		justify-content: center;
		flex: none;
	}
	.gc-text {
		flex: 1;
	}
	.gc-btn {
		padding: 7px 12px;
		border-radius: 9px;
		font-size: 13px;
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
