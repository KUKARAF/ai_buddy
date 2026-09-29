// Presentation helpers that turn a backend Goal (or a bare category label) into
// the visual props the cards need — a pastel tile colour + a matching glyph —
// derived deterministically so a given goal always looks the same.

import type { Goal } from './api/client';

const TILES = ['peach', 'lavender', 'sage', 'sky'] as const;
export type Tile = (typeof TILES)[number];

export interface Look {
	tile: Tile;
	icon: string;
	category: string;
}

/** The category options offered in the wizard / used across the app. */
export const CATEGORIES = [
	'Creative',
	'Learning',
	'Movement',
	'Health',
	'Career',
	'Other'
] as const;

// Canonical look per category label.
const CATEGORY_LOOKS: Record<string, { tile: Tile; icon: string }> = {
	creative: { tile: 'peach', icon: 'music' },
	learning: { tile: 'lavender', icon: 'book' },
	movement: { tile: 'sage', icon: 'heart' },
	health: { tile: 'sky', icon: 'heart' },
	career: { tile: 'sky', icon: 'target' },
	other: { tile: 'lavender', icon: 'target' }
};

// Keyword -> category, for goals created without an explicit category.
const KEYWORD_RULES: Array<{ match: RegExp; category: string }> = [
	{ match: /crea|music|sing|art|write|paint|draw|photo/i, category: 'Creative' },
	{ match: /learn|study|language|spanish|french|read|book|course|juggl/i, category: 'Learning' },
	{ match: /walk|run|gym|movement|dance|swim|cycl|sport|move/i, category: 'Movement' },
	{ match: /health|fit|sleep|meditat|body|eat|nutrition/i, category: 'Health' },
	{ match: /work|career|money|finance|business|job/i, category: 'Career' }
];

function hashIndex(s: string, mod: number): number {
	let h = 0;
	for (let i = 0; i < s.length; i++) h = (h * 31 + s.charCodeAt(i)) >>> 0;
	return h % mod;
}

/** Visuals for a plain category label. */
export function categoryLook(category: string): Look {
	const key = category.trim().toLowerCase();
	const look = CATEGORY_LOOKS[key];
	if (look) return { ...look, category: category.trim() };
	return { tile: TILES[hashIndex(key, TILES.length)], icon: 'target', category: category.trim() };
}

/** Derive a stable tile/icon/category for a goal. */
export function goalLook(goal: Goal): Look {
	const explicit = goal.category?.trim();
	if (explicit) return categoryLook(explicit);

	const haystack = `${goal.category ?? ''} ${goal.title}`;
	for (const rule of KEYWORD_RULES) {
		if (rule.match.test(haystack)) return categoryLook(rule.category);
	}
	return { tile: TILES[hashIndex(goal.title, TILES.length)], icon: 'target', category: 'Goal' };
}
