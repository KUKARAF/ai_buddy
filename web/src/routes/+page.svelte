<script lang="ts">
	import { onMount } from 'svelte';
	import { getWallet, topup, ApiError, type Wallet } from '$lib/api/client';

	const MIN_CENTS = 400; // €4
	const MAX_CENTS = 10000; // €100

	let wallet = $state<Wallet | null>(null);
	let loading = $state(true);
	let loadError = $state<string | null>(null);

	// Top-up form (euros, as a string so the input stays controllable).
	let amountEuros = $state('4');
	let submitting = $state(false);
	let formError = $state<string | null>(null);
	let success = $state<string | null>(null);

	const amountCents = $derived(Math.round(parseFloat(amountEuros || '0') * 100));
	const balanceEuros = $derived(wallet ? (wallet.balance_cents / 100).toFixed(2) : '—');

	onMount(async () => {
		try {
			wallet = await getWallet();
		} catch (err) {
			loadError = err instanceof ApiError ? err.message : 'Could not load your wallet.';
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
			success = `Topped up €${(amountCents / 100).toFixed(2)}.`;
		} catch (err) {
			formError = err instanceof ApiError ? err.message : 'Top-up failed. Please try again.';
		} finally {
			submitting = false;
		}
	}
</script>

<section class="card">
	<h2>Your wallet</h2>
	{#if loading}
		<p class="muted">Loading balance…</p>
	{:else if loadError}
		<p class="error">{loadError}</p>
	{:else}
		<p class="balance">€{balanceEuros}</p>
	{/if}
</section>

<section class="card">
	<h2>Top up</h2>
	<p class="muted">
		AI Buddy runs on a prepaid wallet. Top up at least <strong>€4</strong> (up to €100). Your balance
		covers the AI's token usage and can be pledged to your goals — hit a goal and the pledge is refunded,
		miss it and it's forfeited.
	</p>

	<form onsubmit={submit}>
		<label for="amount">Amount (€)</label>
		<div class="row">
			<input
				id="amount"
				type="number"
				min="4"
				max="100"
				step="1"
				bind:value={amountEuros}
				disabled={submitting}
			/>
			<button type="submit" disabled={submitting}>
				{submitting ? 'Processing…' : 'Top up'}
			</button>
		</div>

		<div class="presets">
			{#each [4, 10, 25, 50] as preset (preset)}
				<button
					type="button"
					class="preset"
					onclick={() => (amountEuros = String(preset))}
					disabled={submitting}
				>
					€{preset}
				</button>
			{/each}
		</div>

		{#if formError}<p class="error">{formError}</p>{/if}
		{#if success}<p class="success">{success}</p>{/if}
	</form>
</section>

<style>
	.card {
		background: white;
		border: 1px solid #e2e4ee;
		border-radius: 14px;
		padding: 1.25rem 1.25rem 1.5rem;
		margin-bottom: 1.25rem;
	}

	h2 {
		margin: 0 0 0.75rem;
		font-size: 1.1rem;
	}

	.balance {
		font-size: 2.4rem;
		font-weight: 700;
		margin: 0;
		letter-spacing: -0.03em;
	}

	.muted {
		color: #6b6b85;
		font-size: 0.92rem;
		line-height: 1.5;
	}

	form {
		margin-top: 0.5rem;
	}

	label {
		display: block;
		font-size: 0.85rem;
		color: #4a4a6a;
		margin-bottom: 0.35rem;
	}

	.row {
		display: flex;
		gap: 0.5rem;
	}

	input {
		flex: 1;
		padding: 0.6rem 0.7rem;
		border: 1px solid #cfd2e0;
		border-radius: 10px;
		font-size: 1rem;
	}

	button {
		padding: 0.6rem 1.1rem;
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

	.presets {
		display: flex;
		gap: 0.4rem;
		margin-top: 0.6rem;
	}

	.preset {
		background: #eceafe;
		color: #4b3ff2;
		padding: 0.35rem 0.7rem;
		font-size: 0.85rem;
	}

	.error {
		color: #c0263a;
		font-size: 0.9rem;
	}

	.success {
		color: #1c8a4b;
		font-size: 0.9rem;
	}
</style>
