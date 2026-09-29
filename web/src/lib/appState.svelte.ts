// Shared, reactive app state (Svelte 5 runes in a .svelte.ts module).
//
// The layout hydrates `me` (GET /auth/me) and `goals` (GET /api/goals) once on
// mount; every page reads from the same singleton so the sidebar, greeting and
// dashboard stay in sync. A 401 or network error is never fatal — it just means
// "guest": not signed in, with no data. Pages then show honest empty states,
// never fabricated content.

import { getMe, listGoals, getStreak, type Me, type Goal, type Streak } from './api/client';

class AppState {
	me = $state<Me | null>(null);
	goals = $state<Goal[]>([]);
	streak = $state<Streak | null>(null);
	/** True once the initial auth + goals fetch has settled (success or not). */
	loaded = $state(false);
	/** Dedupes concurrent load() calls so auth is fetched exactly once. */
	private loadPromise: Promise<void> | null = null;

	get isGuest(): boolean {
		return this.me === null;
	}

	/**
	 * Resolve the initial auth + data load exactly once, sharing a single
	 * in-flight promise across all callers. Guards call this before reading
	 * `isGuest` so a not-yet-loaded state never misreports a signed-in user.
	 */
	ensureLoaded(): Promise<void> {
		if (this.loadPromise === null) this.loadPromise = this.load();
		return this.loadPromise;
	}

	/** First name for greetings, from display_name; falls back to "there". */
	get firstName(): string {
		const name = this.me?.display_name?.trim();
		if (!name) return 'there';
		return name.split(/\s+/)[0];
	}

	async load(): Promise<void> {
		try {
			this.me = await getMe();
		} catch {
			this.me = null;
		}
		if (this.me !== null) {
			await Promise.all([this.refreshGoals(), this.refreshStreak()]);
		} else {
			this.goals = [];
			this.streak = null;
		}
		this.loaded = true;
	}

	async refreshGoals(): Promise<void> {
		if (this.me === null) return;
		try {
			this.goals = await listGoals();
		} catch {
			/* keep previous goals on transient failure */
		}
	}

	async refreshStreak(): Promise<void> {
		if (this.me === null) return;
		try {
			this.streak = await getStreak();
		} catch {
			this.streak = null;
		}
	}
}

export const app = new AppState();
