<script lang="ts">
	import { onMount } from 'svelte';
	import Icon from '$lib/components/Icon.svelte';
	import { getSettings, updateSettings, ApiError, type Settings } from '$lib/api/client';
	import { startLogin } from '$lib/app/login';

	let settings = $state<Settings | null>(null);
	let loading = $state(true);
	let loadError = $state<string | null>(null);
	let guest = $state(false);

	let selected = $state('');
	let saving = $state(false);
	let saved = $state(false);
	let saveError = $state<string | null>(null);

	// Hide the "Saved" confirmation after a short beat.
	let savedTimer: ReturnType<typeof setTimeout> | null = null;

	onMount(async () => {
		try {
			settings = await getSettings();
			selected = settings.chat_model;
		} catch (err) {
			if (err instanceof ApiError && err.status === 401) {
				guest = true;
			} else {
				loadError = err instanceof ApiError ? err.message : 'Could not load your settings.';
			}
		} finally {
			loading = false;
		}
	});

	async function onChange(next: string) {
		if (next === selected || saving) return;
		const previous = selected;
		selected = next;
		saving = true;
		saved = false;
		saveError = null;
		if (savedTimer) clearTimeout(savedTimer);

		try {
			const result = await updateSettings(next);
			selected = result.chat_model;
			if (settings) settings.chat_model = result.chat_model;
			saved = true;
			savedTimer = setTimeout(() => (saved = false), 2200);
		} catch (err) {
			selected = previous; // revert the pending choice
			saveError =
				err instanceof ApiError
					? err.status === 400
						? 'That model isn’t available. Please pick another.'
						: err.message
					: 'Could not save your choice. Please try again.';
		} finally {
			saving = false;
		}
	}
</script>

<div class="page">
	<div class="head">
		<h1>Settings</h1>
		<p class="muted">Tune how Buddy works for you.</p>
	</div>

	{#if guest}
		<div class="card signin-card">
			<span class="tile tile-sky"><Icon name="gear" size={24} /></span>
			<h2>Sign in to change your settings</h2>
			<p class="muted">Your preferences are private to you.</p>
			<button class="btn btn-lime" onclick={() => void startLogin()}>Sign in</button>
		</div>
	{:else}
		<section class="card model-card">
			<div class="section-head">
				<h2>AI model</h2>
				<p class="muted">Choose the model your coach uses for chat and roadmaps.</p>
			</div>

			{#if loading}
				<p class="muted">Loading…</p>
			{:else if loadError}
				<p class="error">{loadError}</p>
			{:else if settings && settings.allowed_models.length > 0}
				<fieldset class="models" disabled={saving}>
					<legend class="sr-only">AI model</legend>
					{#each settings.allowed_models as option (option.id)}
						<label class="model" class:on={selected === option.id}>
							<input
								type="radio"
								name="chat_model"
								value={option.id}
								checked={selected === option.id}
								onchange={() => onChange(option.id)}
							/>
							<span class="model-body">
								<span class="model-label">{option.label}</span>
								<span class="model-id muted small">{option.id}</span>
							</span>
							<span class="check" aria-hidden="true">
								{#if selected === option.id}<Icon name="check" size={16} stroke={2.4} />{/if}
							</span>
						</label>
					{/each}
				</fieldset>

				<div class="status" aria-live="polite">
					{#if saving}
						<span class="muted small">Saving…</span>
					{:else if saveError}
						<span class="error small">{saveError}</span>
					{:else if saved}
						<span class="ok small"><Icon name="check" size={14} stroke={2.6} /> Saved</span>
					{/if}
				</div>
			{:else}
				<p class="muted">No models are available to choose right now.</p>
			{/if}
		</section>
	{/if}
</div>

<style>
	.page {
		display: flex;
		flex-direction: column;
		gap: 20px;
		max-width: 640px;
	}
	.head h1 {
		font-size: 30px;
	}
	.head p {
		margin: 6px 0 0;
		font-size: 15px;
	}
	.model-card {
		padding: 22px;
		display: flex;
		flex-direction: column;
		gap: 16px;
	}
	.section-head h2 {
		font-size: 18px;
	}
	.section-head p {
		margin: 4px 0 0;
		font-size: 14px;
	}
	.models {
		border: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 10px;
	}
	.model {
		display: flex;
		align-items: center;
		gap: 12px;
		border: 1px solid var(--line);
		border-radius: 12px;
		padding: 14px 16px;
		cursor: pointer;
		background: var(--bg);
	}
	.model:hover {
		border-color: var(--muted);
	}
	.model.on {
		border-color: var(--teal);
		background: color-mix(in srgb, var(--lime) 16%, var(--card));
	}
	.model input {
		position: absolute;
		opacity: 0;
		width: 0;
		height: 0;
	}
	.model-body {
		display: flex;
		flex-direction: column;
		gap: 2px;
		flex: 1;
		min-width: 0;
	}
	.model-label {
		font-weight: 600;
		font-size: 15px;
	}
	.model-id {
		word-break: break-all;
	}
	.small {
		font-size: 12px;
	}
	.check {
		width: 22px;
		height: 22px;
		border-radius: 999px;
		background: var(--teal);
		color: var(--lime);
		display: inline-flex;
		align-items: center;
		justify-content: center;
		flex: none;
		opacity: 0;
	}
	.model.on .check {
		opacity: 1;
	}
	.status {
		min-height: 20px;
	}
	.ok {
		display: inline-flex;
		align-items: center;
		gap: 5px;
		color: var(--sage-ink);
		font-weight: 600;
	}
	.error {
		color: #c0263a;
		font-size: 14px;
		margin: 0;
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
	.sr-only {
		position: absolute;
		width: 1px;
		height: 1px;
		padding: 0;
		margin: -1px;
		overflow: hidden;
		clip: rect(0, 0, 0, 0);
		white-space: nowrap;
		border: 0;
	}
</style>
