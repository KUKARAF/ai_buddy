<script lang="ts">
	import { onMount } from 'svelte';
	import Icon from '$lib/components/Icon.svelte';
	import { getWallet, topup, ApiError, type Wallet } from '$lib/api/client';
	import { startLogin } from '$lib/app/login';
	import { app } from '$lib/appState.svelte';
	import { euros } from '$lib/format';

	const MIN_CENTS = 400;
	const MAX_CENTS = 10000;

	let wallet = $state<Wallet | null>(null);
	let loading = $state(true);
	let loadError = $state<string | null>(null);
	let guest = $state(false);

	let amountEuros = $state('4');
	let submitting = $state(false);
	let formError = $state<string | null>(null);
	let success = $state<string | null>(null);

	const amountCents = $derived(Math.round(parseFloat(amountEuros || '0') * 100));
	const balance = $derived(wallet ? euros(wallet.balance_cents) : '—');

	onMount(async () => {
		try {
			wallet = await getWallet();
		} catch (err) {
			if (err instanceof ApiError && err.status === 401) {
				guest = true;
			} else {
				loadError = err instanceof ApiError ? err.message : 'Could not load your wallet.';
			}
		} finally {
			loading = false;
		}
	});

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		formError = null;
		success = null;

		if (!Number.isFinite(amountCents) || amountCents < MIN_CENTS) {
			formError = 'Minimum top-up is €4.';
			return;
		}
		if (amountCents > MAX_CENTS) {
			formError = 'Maximum balance is €100.';
			return;
		}

		submitting = true;
		try {
			wallet = await topup(amountCents);
			success = `Added ${euros(amountCents)} to your wallet.`;
		} catch (err) {
			formError = err instanceof ApiError ? err.message : 'Top-up failed. Please try again.';
		} finally {
			submitting = false;
		}
	}
</script>

<div class="page">
	<div class="head">
		<h1>Your wallet</h1>
		<p class="muted">
			A small prepaid balance covers your coaching — and what you pledge to your goals.
		</p>
	</div>

	{#if guest}
		<div class="card signin-card">
			<span class="tile tile-sky"><Icon name="wallet" size={24} /></span>
			<h2>Sign in to see your balance</h2>
			<p class="muted">Your wallet is private to you.</p>
			<button class="btn btn-lime" onclick={() => void startLogin()}>Sign in</button>
		</div>
	{:else}
		<div class="row-cards">
			<section class="card balance-card">
				<p class="eyebrow">Current balance</p>
				{#if loading}
					<p class="balance muted">…</p>
				{:else if loadError}
					<p class="error">{loadError}</p>
				{:else}
					<p class="balance">{balance}</p>
					<p class="muted small">Signed in as {app.me?.display_name ?? app.me?.email ?? 'you'}.</p>
				{/if}
			</section>

			<section class="card topup-card">
				<h2>Top up</h2>
				<form onsubmit={submit}>
					<div class="chips">
						{#each [4, 10, 25] as preset (preset)}
							<button
								type="button"
								class="chip"
								class:on={amountEuros === String(preset)}
								onclick={() => (amountEuros = String(preset))}
								disabled={submitting}
							>
								€{preset}
							</button>
						{/each}
					</div>
					<label for="amount">Amount (€4–€100)</label>
					<div class="inputrow">
						<span class="cur">€</span>
						<input
							id="amount"
							type="number"
							min="4"
							max="100"
							step="1"
							bind:value={amountEuros}
							disabled={submitting}
						/>
						<button type="submit" class="btn" disabled={submitting}>
							{submitting ? 'Processing…' : 'Top up'}
						</button>
					</div>
					{#if formError}<p class="error">{formError}</p>{/if}
					{#if success}<p class="success">{success}</p>{/if}
					<p class="muted small note">Top-ups may use a test/mock flow while we finish payments.</p>
				</form>
			</section>
		</div>
	{/if}

	<section class="card explainer">
		<span class="tile tile-sage"><Icon name="heart" size={22} /></span>
		<div>
			<h2>Put a little belief behind it</h2>
			<p class="muted">
				A pledge is a small amount you place on a goal. It's held while you work toward it —
				refunded when you follow through, and gently forfeited if you let it go. A quiet way to tell
				yourself this one matters.
			</p>
		</div>
	</section>
</div>

<style>
	.page {
		display: flex;
		flex-direction: column;
		gap: 20px;
		max-width: 860px;
	}
	.head h1 {
		font-size: 30px;
	}
	.head p {
		margin: 6px 0 0;
		font-size: 15px;
		max-width: 60ch;
	}
	.row-cards {
		display: grid;
		grid-template-columns: 1fr 1.4fr;
		gap: 16px;
	}
	.balance-card {
		padding: 22px;
		display: flex;
		flex-direction: column;
		gap: 6px;
	}
	.balance {
		font-size: 40px;
		font-weight: 700;
		letter-spacing: -0.03em;
		margin: 6px 0 0;
	}
	.topup-card {
		padding: 22px;
	}
	.topup-card h2 {
		font-size: 18px;
		margin-bottom: 14px;
	}
	.chips {
		display: flex;
		gap: 8px;
		margin-bottom: 14px;
	}
	.chip {
		background: var(--tag-bg);
		color: var(--ink);
		border: 1px solid var(--line);
		border-radius: 999px;
		padding: 7px 16px;
		font-size: 14px;
		font-weight: 600;
	}
	.chip.on {
		background: var(--lime);
		border-color: var(--lime);
	}
	label {
		display: block;
		font-size: 13px;
		color: var(--muted);
		margin-bottom: 6px;
	}
	.inputrow {
		display: flex;
		align-items: center;
		gap: 8px;
		border: 1px solid var(--line);
		border-radius: 12px;
		padding: 4px 4px 4px 12px;
		background: var(--bg);
	}
	.cur {
		color: var(--muted);
		font-weight: 600;
	}
	input {
		flex: 1;
		border: none;
		background: transparent;
		outline: none;
		font-size: 16px;
		font-family: inherit;
		color: var(--ink);
		width: 100%;
		min-width: 0;
	}
	.inputrow .btn {
		flex: none;
	}
	.note {
		margin-top: 12px;
	}
	.small {
		font-size: 12px;
	}
	.explainer {
		display: flex;
		gap: 16px;
		align-items: flex-start;
		padding: 22px;
	}
	.explainer h2 {
		font-size: 18px;
		margin-bottom: 6px;
	}
	.explainer p {
		margin: 0;
		max-width: 62ch;
	}
	.signin-card {
		display: flex;
		flex-direction: column;
		align-items: center;
		text-align: center;
		gap: 12px;
		padding: 44px 24px;
	}
	.signin-card h2 {
		font-size: 20px;
	}
	.signin-card p {
		margin: 0;
	}
	.error {
		color: #c0263a;
		font-size: 14px;
		margin: 8px 0 0;
	}
	.success {
		color: #1c8a4b;
		font-size: 14px;
		margin: 8px 0 0;
	}
	@media (max-width: 700px) {
		.row-cards {
			grid-template-columns: 1fr;
		}
	}
</style>
