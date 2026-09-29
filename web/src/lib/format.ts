// Small pure formatting helpers shared across pages.

/** Format integer cents as euros, e.g. 1234 -> "€12.34". */
export function euros(cents: number): string {
	return `€${(cents / 100).toFixed(2)}`;
}

const MONTHS = [
	'January',
	'February',
	'March',
	'April',
	'May',
	'June',
	'July',
	'August',
	'September',
	'October',
	'November',
	'December'
];

const DAYS = ['SUNDAY', 'MONDAY', 'TUESDAY', 'WEDNESDAY', 'THURSDAY', 'FRIDAY', 'SATURDAY'];

/** Caps date eyebrow, e.g. "TUESDAY 29 SEPTEMBER". */
export function dateEyebrow(d: Date = new Date()): string {
	return `${DAYS[d.getDay()]} ${d.getDate()} ${MONTHS[d.getMonth()].toUpperCase()}`;
}

/** Short "finish line" style date, e.g. "12 Dec". Returns null for empty input. */
export function shortDate(iso: string | null): string | null {
	if (!iso) return null;
	const d = new Date(iso);
	if (Number.isNaN(d.getTime())) return null;
	const month = MONTHS[d.getMonth()].slice(0, 3);
	return `${d.getDate()} ${month}`;
}

/** Human relative time, e.g. "just now", "3h ago", "2d ago", or a short date. */
export function relativeDate(iso: string): string {
	const d = new Date(iso);
	if (Number.isNaN(d.getTime())) return '';
	const diffMs = Date.now() - d.getTime();
	const min = Math.round(diffMs / 60000);
	if (min < 1) return 'just now';
	if (min < 60) return `${min}m ago`;
	const hrs = Math.round(min / 60);
	if (hrs < 24) return `${hrs}h ago`;
	const days = Math.round(hrs / 24);
	if (days < 7) return `${days}d ago`;
	return `${d.getDate()} ${MONTHS[d.getMonth()].slice(0, 3)}`;
}
