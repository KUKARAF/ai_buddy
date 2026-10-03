// A one-shot confetti burst for small wins (e.g. completing a milestone).
//
// `canvas-confetti` is a browser-only library (it touches `window`/`document`),
// so it MUST be dynamically imported client-side — this app is prerendered with
// SSR off, and a top-level import would break the build. We also respect the
// user's reduced-motion preference and never fire during SSR.
//
// Pass the element the burst should originate from (e.g. the tapped milestone
// node) and the confetti fans out from its centre; omit it to fire from a
// sensible default near the middle of the screen.
export async function celebrate(originEl?: HTMLElement): Promise<void> {
	if (typeof window === 'undefined') return;
	if (window.matchMedia?.('(prefers-reduced-motion: reduce)').matches) return;
	const { default: confetti } = await import('canvas-confetti');
	let origin = { x: 0.5, y: 0.6 };
	if (originEl) {
		const r = originEl.getBoundingClientRect();
		origin = {
			x: (r.left + r.width / 2) / window.innerWidth,
			y: (r.top + r.height / 2) / window.innerHeight
		};
	}
	confetti({
		particleCount: 120,
		spread: 70,
		startVelocity: 45,
		origin,
		disableForReducedMotion: true
	});
}
