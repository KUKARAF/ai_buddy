// Shared, reactive notifications state (Svelte 5 runes in a .svelte.ts module).
//
// Mirrors appState.svelte.ts: a singleton the layout hydrates once (client-side)
// and the notifications bell reads from. A 401 or network error is never fatal —
// it just means "guest" / nothing to show, so we fall back to an empty list and
// never throw. Marking read is optimistic: we flip `read_at` locally right away
// and reconcile with the server in the background.

import {
	listNotifications,
	markNotificationRead,
	markAllNotificationsRead,
	type Notification
} from './api/client';

class NotificationsState {
	items = $state<Notification[]>([]);
	/** True once an initial refresh has settled (success or not). */
	loaded = $state(false);

	/** Count of unread notifications (read_at === null). */
	get unread(): number {
		return this.items.filter((n) => n.read_at === null).length;
	}

	/**
	 * Fetch the latest notifications. Guests (401) and transient failures resolve
	 * to an empty list rather than throwing, so callers can fire-and-forget.
	 */
	async refresh(): Promise<void> {
		try {
			this.items = await listNotifications();
		} catch {
			// 401 (guest) or network error — show nothing, never a stale/fake list.
			this.items = [];
		} finally {
			this.loaded = true;
		}
	}

	/** Optimistically mark one notification read, then persist it. */
	async markRead(id: string): Promise<void> {
		const target = this.items.find((n) => n.id === id);
		if (!target || target.read_at !== null) return;
		const now = new Date().toISOString();
		this.items = this.items.map((n) => (n.id === id ? { ...n, read_at: now } : n));
		try {
			await markNotificationRead(id);
		} catch {
			/* leave the optimistic state; a later refresh reconciles */
		}
	}

	/** Optimistically mark every notification read, then persist it. */
	async markAllRead(): Promise<void> {
		if (this.unread === 0) return;
		const now = new Date().toISOString();
		this.items = this.items.map((n) => (n.read_at === null ? { ...n, read_at: now } : n));
		try {
			await markAllNotificationsRead();
		} catch {
			/* leave the optimistic state; a later refresh reconciles */
		}
	}
}

export const notifications = new NotificationsState();
