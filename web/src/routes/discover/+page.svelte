<script lang="ts">
	import { resolve } from '$app/paths';
	import Icon from '$lib/components/Icon.svelte';
	import { wizard } from '$lib/wizardState.svelte';
	import { categoryLook } from '$lib/goalView';

	interface Idea {
		title: string;
		category: string;
		tagline: string;
	}

	const IDEAS: Idea[] = [
		{
			title: 'Sing a song at my wedding',
			category: 'Creative',
			tagline: 'One song, learned line by line, ready for the big day.'
		},
		{
			title: 'Have a conversation in Spanish',
			category: 'Learning',
			tagline: 'From first words to a real back-and-forth chat.'
		},
		{
			title: 'Learn to juggle three balls',
			category: 'Learning',
			tagline: 'A playful skill you can build in ten quiet minutes a day.'
		},
		{
			title: 'Build a consistent walking habit',
			category: 'Movement',
			tagline: 'Small daily steps that add up to real momentum.'
		},
		{
			title: 'Sleep at a steady hour',
			category: 'Health',
			tagline: 'Wind down gently and wake up feeling like yourself.'
		},
		{
			title: 'Ship a tiny side project',
			category: 'Career',
			tagline: 'One small thing, finished and out in the world.'
		}
	];

	function makeGoal(idea: Idea): void {
		wizard.openWizard({ title: idea.title, category: idea.category });
	}
</script>

<div class="page">
	<div class="head">
		<h1>Discover</h1>
		<p class="muted">A few someday ideas to borrow. Pick one and make it yours.</p>
	</div>

	<div class="gallery">
		{#each IDEAS as idea (idea.title)}
			{@const look = categoryLook(idea.category)}
			<article class="card idea tile-bg-{look.tile}">
				<div class="idea-top">
					<span class="tile tile-{look.tile}"><Icon name={look.icon} size={22} /></span>
					<span class="pill">{look.category}</span>
				</div>
				<h3>{idea.title}</h3>
				<p class="muted">{idea.tagline}</p>
				<button class="btn make" onclick={() => makeGoal(idea)}>
					Make this my goal <Icon name="plus" size={15} />
				</button>
			</article>
		{/each}
	</div>

	<section class="card circles-strip">
		<div>
			<p class="eyebrow">Circles</p>
			<h3>Take the first steps alongside others.</h3>
			<p class="muted">A way to find people working toward similar goals — coming soon.</p>
		</div>
		<a class="btn btn-ghost" href={resolve('/circles')}>
			<Icon name="people" size={16} /> Explore circles
		</a>
	</section>
</div>

<style>
	.page {
		display: flex;
		flex-direction: column;
		gap: 22px;
	}
	.head h1 {
		font-size: 30px;
	}
	.head p {
		margin: 6px 0 0;
		font-size: 15px;
	}
	.gallery {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
		gap: 16px;
	}
	.idea {
		padding: 20px;
		display: flex;
		flex-direction: column;
		gap: 10px;
	}
	.idea-top {
		display: flex;
		align-items: center;
		justify-content: space-between;
	}
	.idea h3 {
		font-size: 18px;
		line-height: 1.25;
	}
	.idea p {
		margin: 0;
		font-size: 14px;
		flex: 1;
	}
	.make {
		align-self: flex-start;
		margin-top: 4px;
	}
	.tile-bg-peach {
		background: color-mix(in srgb, var(--peach) 16%, var(--card));
	}
	.tile-bg-lavender {
		background: color-mix(in srgb, var(--lavender) 20%, var(--card));
	}
	.tile-bg-sage {
		background: color-mix(in srgb, var(--sage) 22%, var(--card));
	}
	.tile-bg-sky {
		background: color-mix(in srgb, var(--sky) 22%, var(--card));
	}
	.circles-strip {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 16px;
		padding: 22px;
	}
	.circles-strip h3 {
		font-size: 18px;
		margin: 4px 0;
	}
	.circles-strip p {
		margin: 0;
		font-size: 14px;
	}
	@media (max-width: 620px) {
		.circles-strip {
			flex-direction: column;
			align-items: flex-start;
		}
	}
</style>
