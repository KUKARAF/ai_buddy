// Dependency-free iCalendar (.ics) builder for the goal plan's "add to calendar"
// affordances (per-milestone, the finish line, or the whole plan at once).
//
// Everything here is all-day VEVENTs built entirely client-side — no backend,
// no Google integration. The download is triggered via a temporary <a download>.
//
// NOTE: in the Tauri APK a blob download may not trigger (the webview has no
// real download handler). Web is the target here and we deliberately don't
// special-case the app.

/** The UID domain for events (keeps UIDs stable + globally unique per entity). */
const UID_DOMAIN = 'buddy.osmosis.page';

/** One all-day calendar event. */
export interface IcsEvent {
	/** Stable id (goal/step id) — becomes the VEVENT UID. */
	uid: string;
	/** Event day: a `YYYY-MM-DD` or full ISO date string. Invalid dates are skipped. */
	date: string;
	/** VEVENT SUMMARY (milestone / goal title). */
	summary: string;
	/** Optional VEVENT DESCRIPTION (goal title / detail). */
	description?: string;
}

/**
 * Escape a text value per RFC 5545 §3.3.11: backslash, semicolon and comma are
 * escaped, and newlines become the literal `\n`. Order matters — backslashes
 * first so we don't double-escape the ones we introduce.
 */
export function escapeIcsText(value: string): string {
	return value
		.replace(/\\/g, '\\\\')
		.replace(/;/g, '\\;')
		.replace(/,/g, '\\,')
		.replace(/\r\n|\r|\n/g, '\\n');
}

const pad = (n: number): string => String(n).padStart(2, '0');

/** Format a Date as a UTC DTSTAMP, e.g. `20261003T091500Z`. */
function formatStamp(d: Date): string {
	return (
		`${d.getUTCFullYear()}${pad(d.getUTCMonth() + 1)}${pad(d.getUTCDate())}` +
		`T${pad(d.getUTCHours())}${pad(d.getUTCMinutes())}${pad(d.getUTCSeconds())}Z`
	);
}

/** Turn a `YYYY-MM-DD` (or full ISO) string into a DATE value `YYYYMMDD`, or null. */
function toDateValue(iso: string): string | null {
	const m = /^(\d{4})-(\d{2})-(\d{2})/.exec(iso);
	if (m) return `${m[1]}${m[2]}${m[3]}`;
	const d = new Date(iso);
	if (Number.isNaN(d.getTime())) return null;
	return `${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}`;
}

/** The day after a `YYYYMMDD` value — all-day DTEND is exclusive. */
function nextDay(dateValue: string): string {
	const y = Number(dateValue.slice(0, 4));
	const mo = Number(dateValue.slice(4, 6));
	const da = Number(dateValue.slice(6, 8));
	const d = new Date(Date.UTC(y, mo - 1, da));
	d.setUTCDate(d.getUTCDate() + 1);
	return `${d.getUTCFullYear()}${pad(d.getUTCMonth() + 1)}${pad(d.getUTCDate())}`;
}

/** Build one VEVENT (as lines) for an all-day event, or null if the date is invalid. */
function buildEvent(ev: IcsEvent, stamp: string): string[] | null {
	const start = toDateValue(ev.date);
	if (start === null) return null;
	const lines = [
		'BEGIN:VEVENT',
		`UID:${ev.uid}@${UID_DOMAIN}`,
		`DTSTAMP:${stamp}`,
		`DTSTART;VALUE=DATE:${start}`,
		`DTEND;VALUE=DATE:${nextDay(start)}`,
		`SUMMARY:${escapeIcsText(ev.summary)}`
	];
	if (ev.description) lines.push(`DESCRIPTION:${escapeIcsText(ev.description)}`);
	lines.push('END:VEVENT');
	return lines;
}

/**
 * Build a full VCALENDAR document from one or more events. Events with an
 * unparseable date are silently dropped; the result is still a valid calendar.
 * Lines are joined with CRLF as the spec requires.
 */
export function buildIcs(events: IcsEvent[]): string {
	const stamp = formatStamp(new Date());
	const body: string[] = [];
	for (const ev of events) {
		const block = buildEvent(ev, stamp);
		if (block) body.push(...block);
	}
	const lines = ['BEGIN:VCALENDAR', 'VERSION:2.0', 'PRODID:-//buddy//EN', ...body, 'END:VCALENDAR'];
	return lines.join('\r\n');
}

/** Trigger a client-side `.ics` download via a temporary `<a download>`. */
export function downloadIcs(filename: string, ics: string): void {
	const blob = new Blob([ics], { type: 'text/calendar;charset=utf-8' });
	const url = URL.createObjectURL(blob);
	const a = document.createElement('a');
	a.href = url;
	a.download = filename.endsWith('.ics') ? filename : `${filename}.ics`;
	document.body.appendChild(a);
	a.click();
	a.remove();
	// Revoke on the next tick so the browser has started the download first.
	setTimeout(() => URL.revokeObjectURL(url), 0);
}

/** A safe, short `.ics` filename slug derived from a title. */
export function icsFilename(title: string): string {
	const slug = title
		.toLowerCase()
		.replace(/[^a-z0-9]+/g, '-')
		.replace(/^-+|-+$/g, '')
		.slice(0, 40);
	return `${slug || 'event'}.ics`;
}
