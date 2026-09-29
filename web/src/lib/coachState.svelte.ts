// Cross-surface handoff for the agentic goal coach at /chat.
//
// Every "create a goal" entry point (top-bar "New goal", the hero + empty
// states, the /goals add-tile, and Discover's "Make this my goal") navigates to
// /chat instead of opening a form. This tiny singleton lets those surfaces tell
// the chat page how to open:
//   - `startNew()`   — drop any active conversation so the coach opens fresh.
//   - `seedNew(text)`— open fresh AND auto-send `text` as the first user message
//                       (Discover uses this to seed the coach with an idea).
// The chat page consumes `pendingSeed` exactly once on load (via `takeSeed()`),
// and keeps `conversationId` so an in-session return to /chat resumes history.

class CoachState {
	/** The active coach conversation id, or null to start fresh. */
	conversationId = $state<string | null>(null);
	/** A one-shot first message to auto-send on the next /chat load, if any. */
	pendingSeed = $state<string | null>(null);

	/** Open the coach on a brand-new conversation (no seed). */
	startNew(): void {
		this.conversationId = null;
		this.pendingSeed = null;
	}

	/** Open the coach fresh and auto-send `text` as the first user message. */
	seedNew(text: string): void {
		this.conversationId = null;
		this.pendingSeed = text;
	}

	/** Read and clear the pending seed (consumed once by the chat page). */
	takeSeed(): string | null {
		const seed = this.pendingSeed;
		this.pendingSeed = null;
		return seed;
	}
}

export const coach = new CoachState();
