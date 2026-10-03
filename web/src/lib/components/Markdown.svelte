<script lang="ts">
	// Renders a coach message as SANITIZED HTML from Markdown.
	//
	// Pipeline: marked (raw HTML left disabled) -> DOMPurify with a tight
	// allowlist. marked still lets raw HTML in the source pass through, so the
	// DOMPurify pass is what actually makes this safe — never skip it. DOMPurify
	// needs a real DOM, so it only runs in the browser; during prerender (no
	// `window`) we fall back to the raw text rendered as escaped plain text.
	import { browser } from '$app/environment';
	import { marked } from 'marked';
	import DOMPurify from 'dompurify';

	interface Props {
		/** Markdown source (e.g. an assistant/coach message). */
		source: string;
	}
	let { source }: Props = $props();

	// Tight allowlist: inline emphasis, links, lists, code, quotes, small headings.
	const ALLOWED_TAGS = [
		'p',
		'br',
		'strong',
		'em',
		'ul',
		'ol',
		'li',
		'a',
		'code',
		'pre',
		'blockquote',
		'h3',
		'h4'
	];
	const ALLOWED_ATTR = ['href'];
	// Restrict link URLs to http(s) + mailto only (no javascript:, data:, etc.).
	const ALLOWED_URI_REGEXP = /^(?:https?:|mailto:)/i;

	// Open links in a new tab, safely. Registered once, lazily, in the browser.
	let hookAdded = false;
	function ensureHook(): void {
		if (hookAdded) return;
		hookAdded = true;
		DOMPurify.addHook('afterSanitizeAttributes', (node) => {
			if (node.tagName === 'A' && node.hasAttribute('href')) {
				node.setAttribute('target', '_blank');
				node.setAttribute('rel', 'noopener noreferrer');
			}
		});
	}

	// Sanitized HTML, computed only in the browser. The prerender path renders
	// `source` as escaped text in the template instead (never via {@html}).
	const html = $derived.by(() => {
		if (!browser) return '';
		ensureHook();
		const rendered = marked.parse(source ?? '', { async: false, gfm: true, breaks: true });
		return DOMPurify.sanitize(rendered, {
			ALLOWED_TAGS,
			ALLOWED_ATTR,
			ALLOWED_URI_REGEXP,
			// Belt-and-suspenders: the allowlist above already drops on* and style,
			// but forbid them explicitly too.
			FORBID_ATTR: ['style'],
			ADD_ATTR: ['target']
		});
	});
</script>

{#if browser}
	<!-- eslint-disable-next-line svelte/no-at-html-tags -- content is sanitized by DOMPurify with a tight allowlist above -->
	<div class="md">{@html html}</div>
{:else}
	<div class="md">{source}</div>
{/if}

<style>
	.md {
		/* inherit the bubble's font-size/color/line-height; neutralize any
		   pre-wrap on the container so marked's inter-tag newlines don't show. */
		white-space: normal;
	}
	/* Tight outer margins so a one-line reply isn't bulky. */
	.md > :global(:first-child) {
		margin-top: 0;
	}
	.md > :global(:last-child) {
		margin-bottom: 0;
	}
	.md :global(p) {
		margin: 0 0 0.5em;
	}
	.md :global(ul),
	.md :global(ol) {
		margin: 0.4em 0;
		padding-left: 1.3em;
	}
	.md :global(li) {
		margin: 0.15em 0;
	}
	.md :global(li::marker) {
		color: var(--muted);
	}
	.md :global(a) {
		color: var(--sky-ink);
		text-decoration: underline;
		font-weight: 500;
		word-break: break-word;
	}
	.md :global(strong) {
		font-weight: 600;
	}
	.md :global(em) {
		font-style: italic;
	}
	.md :global(code) {
		background: var(--tag-bg);
		border-radius: 5px;
		padding: 1px 5px;
		font-size: 0.9em;
		font-family: ui-monospace, 'SFMono-Regular', Menlo, Consolas, monospace;
	}
	.md :global(pre) {
		background: var(--tag-bg);
		border: 1px solid var(--line);
		border-radius: 10px;
		padding: 10px 12px;
		margin: 0.5em 0;
		overflow-x: auto;
	}
	.md :global(pre code) {
		background: none;
		padding: 0;
		font-size: 0.85em;
	}
	.md :global(blockquote) {
		margin: 0.5em 0;
		padding: 2px 0 2px 12px;
		border-left: 3px solid var(--line);
		color: var(--muted);
	}
	.md :global(h3) {
		font-size: 1.05em;
		font-weight: 600;
		margin: 0.6em 0 0.3em;
	}
	.md :global(h4) {
		font-size: 1em;
		font-weight: 600;
		margin: 0.6em 0 0.3em;
	}
</style>
