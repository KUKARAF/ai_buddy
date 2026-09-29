<script lang="ts">
	// Inline SVG icon set — no external assets, CSP-safe. Icons inherit
	// `currentColor` and use a 24x24 viewBox with round line caps.
	interface Props {
		name: string;
		size?: number;
		/** Stroke width override. */
		stroke?: number;
		fill?: boolean;
	}
	let { name, size = 20, stroke = 1.8, fill = false }: Props = $props();
</script>

<svg
	width={size}
	height={size}
	viewBox="0 0 24 24"
	fill={fill ? 'currentColor' : 'none'}
	stroke={fill ? 'none' : 'currentColor'}
	stroke-width={stroke}
	stroke-linecap="round"
	stroke-linejoin="round"
	aria-hidden="true"
>
	{#if name === 'check'}
		<path d="M20 6 9 17l-5-5" />
	{:else if name === 'home'}
		<path d="M3 10.5 12 3l9 7.5" />
		<path d="M5 9.5V20h14V9.5" />
	{:else if name === 'target'}
		<circle cx="12" cy="12" r="8" />
		<circle cx="12" cy="12" r="4" />
		<circle cx="12" cy="12" r="0.6" fill="currentColor" stroke="none" />
	{:else if name === 'compass'}
		<circle cx="12" cy="12" r="9" />
		<path d="m15.5 8.5-2 5-5 2 2-5z" />
	{:else if name === 'people'}
		<circle cx="9" cy="9" r="3" />
		<path d="M3.5 19a5.5 5.5 0 0 1 11 0" />
		<path d="M16 6.2a3 3 0 0 1 0 5.6" />
		<path d="M17.5 13.5a5.5 5.5 0 0 1 3 5" />
	{:else if name === 'sparkle'}
		<path
			d="M12 3c.6 3.6 1.8 4.8 5.4 5.4-3.6.6-4.8 1.8-5.4 5.4-.6-3.6-1.8-4.8-5.4-5.4C10.2 7.8 11.4 6.6 12 3Z"
		/>
		<path
			d="M18 14c.3 1.6.9 2.2 2.5 2.5-1.6.3-2.2.9-2.5 2.5-.3-1.6-.9-2.2-2.5-2.5 1.6-.3 2.2-.9 2.5-2.5Z"
		/>
	{:else if name === 'gear'}
		<circle cx="12" cy="12" r="3" />
		<path
			d="M19.4 12a7.4 7.4 0 0 0-.1-1.2l2-1.5-2-3.4-2.3 1a7.3 7.3 0 0 0-2-1.2L16.6 3H9.4l-.4 2.5a7.3 7.3 0 0 0-2 1.2l-2.3-1-2 3.4 2 1.5A7.4 7.4 0 0 0 4.6 12c0 .4 0 .8.1 1.2l-2 1.5 2 3.4 2.3-1c.6.5 1.3.9 2 1.2l.4 2.5h7.2l.4-2.5c.7-.3 1.4-.7 2-1.2l2.3 1 2-3.4-2-1.5c.1-.4.1-.8.1-1.2Z"
		/>
	{:else if name === 'flame'}
		<path
			d="M12 3s5 3.5 5 8.5A5 5 0 0 1 7 12c0-1.6.7-2.9 1.5-3.8C8.8 9 9.5 9.6 10.5 9.6 10.5 7 12 5 12 3Z"
			fill="currentColor"
			stroke="none"
		/>
	{:else if name === 'arrow-ur'}
		<path d="M7 17 17 7" />
		<path d="M8 7h9v9" />
	{:else if name === 'clock'}
		<circle cx="12" cy="12" r="8.5" />
		<path d="M12 7.5V12l3 2" />
	{:else if name === 'calendar'}
		<rect x="4" y="5" width="16" height="16" rx="2.5" />
		<path d="M4 9.5h16M8 3.5v3M16 3.5v3" />
	{:else if name === 'lock'}
		<rect x="5" y="11" width="14" height="9" rx="2.2" />
		<path d="M8 11V8a4 4 0 0 1 8 0v3" />
	{:else if name === 'music'}
		<path d="M9 18V6l10-2v11" />
		<circle cx="6.5" cy="18" r="2.5" />
		<circle cx="16.5" cy="15" r="2.5" />
	{:else if name === 'book'}
		<path d="M5 4.5h9a3 3 0 0 1 3 3V20a2.5 2.5 0 0 0-2.5-2.5H5Z" />
		<path d="M5 4.5v13" />
	{:else if name === 'plus'}
		<path d="M12 5v14M5 12h14" />
	{:else if name === 'send'}
		<path d="M4 12 20 4l-4 16-4-7-8-1Z" />
	{:else if name === 'chevron-r'}
		<path d="m9 6 6 6-6 6" />
	{:else if name === 'wallet'}
		<rect x="3.5" y="6" width="17" height="13" rx="3" />
		<path d="M3.5 10h17" />
		<circle cx="16.5" cy="13.5" r="1.1" fill="currentColor" stroke="none" />
	{:else if name === 'menu'}
		<path d="M4 7h16M4 12h16M4 17h16" />
	{:else if name === 'heart'}
		<path d="M12 20s-7-4.3-7-9.2A3.8 3.8 0 0 1 12 8a3.8 3.8 0 0 1 7-2.9c0 4.9-7 9.9-7 9.9Z" />
	{:else if name === 'x'}
		<path d="M6 6l12 12M18 6 6 18" />
	{:else if name === 'mic'}
		<rect x="9" y="3" width="6" height="11" rx="3" />
		<path d="M5.5 11a6.5 6.5 0 0 0 13 0M12 17.5V21M8.5 21h7" />
	{:else if name === 'chevron-l'}
		<path d="m15 6-6 6 6 6" />
	{:else if name === 'pin'}
		<path d="M12 21s6-5.3 6-10a6 6 0 0 0-12 0c0 4.7 6 10 6 10Z" />
		<circle cx="12" cy="11" r="2.2" />
	{:else if name === 'sprout'}
		<path d="M12 20v-8" />
		<path d="M12 12c0-3 2.2-5 5-5 0 3-2.2 5-5 5Z" />
		<path d="M12 13c0-2.6-2-4.5-4.5-4.5 0 2.6 2 4.5 4.5 4.5Z" />
	{/if}
</svg>
