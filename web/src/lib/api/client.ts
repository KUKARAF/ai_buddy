// Minimal fetch wrapper + typed helpers for talking to the AI Buddy backend.
//
// The base URL can be overridden at build time via `PUBLIC_API_BASE_URL` (see
// Vite's `import.meta.env`) — set in `.env.development` so `npm run dev` keeps
// talking to the separately-running backend on :8080. When unset (the
// production build: the backend serves these static assets itself, same origin,
// no separate dev server), it falls back to `window.location.origin` so the
// built bundle isn't hardcoded to any one domain. This app is `ssr = false`, so
// `window` is always available by the time these run.
export const API_BASE_URL: string =
	(import.meta.env.PUBLIC_API_BASE_URL as string | undefined) ??
	(typeof window !== 'undefined' ? window.location.origin : '');

import { getDeviceToken } from './deviceToken';

/** Thrown by the request helpers below on any non-2xx response or network failure. */
export class ApiError extends Error {
	/** HTTP status code, or 0 if the request never reached the server (network error). */
	readonly status: number;
	/** Parsed JSON error body, if the response had one and was JSON. */
	readonly body: unknown;

	constructor(message: string, status: number, body?: unknown) {
		super(message);
		this.name = 'ApiError';
		this.status = status;
		this.body = body;
	}
}

export interface RequestOptions {
	/** Extra headers to merge into the request. */
	headers?: Record<string, string>;
	/** AbortSignal for cancellation. */
	signal?: AbortSignal;
}

export async function request<TResponse>(
	method: 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE',
	path: string,
	body?: unknown,
	options: RequestOptions = {}
): Promise<TResponse> {
	const url = path.startsWith('http') ? path : `${API_BASE_URL}${path}`;
	const deviceToken = getDeviceToken();

	let response: Response;
	try {
		response = await fetch(url, {
			method,
			// Session auth is cookie-based (set by the backend after OIDC login),
			// so credentials must be included on every request, including
			// cross-origin ones (e.g. Tauri webview talking to a remote host).
			credentials: 'include',
			headers: {
				...(body !== undefined ? { 'Content-Type': 'application/json' } : {}),
				Accept: 'application/json',
				// App build: authenticate with the stored device token (cookies
				// don't reliably cross the tauri.localhost <-> backend origin
				// split). Spread before options.headers so callers can override.
				...(deviceToken !== null ? { Authorization: `Bearer ${deviceToken}` } : {}),
				...options.headers
			},
			body: body !== undefined ? JSON.stringify(body) : undefined,
			signal: options.signal
		});
	} catch (cause) {
		throw new ApiError(
			`Network error while requesting ${method} ${url}`,
			0,
			cause instanceof Error ? cause.message : cause
		);
	}

	const contentType = response.headers.get('content-type') ?? '';
	const isJson = contentType.includes('application/json');
	const payload = isJson ? await response.json().catch(() => undefined) : undefined;

	if (!response.ok) {
		const message =
			(isJson && payload && typeof payload === 'object' && 'message' in payload
				? String((payload as Record<string, unknown>).message)
				: undefined) ?? `Request failed with status ${response.status}`;
		throw new ApiError(message, response.status, payload);
	}

	return payload as TResponse;
}

// --- Domain types (mirror docs/ARCHITECTURE.md; money is INTEGER cents) --------

export interface Wallet {
	balance_cents: number;
}

export interface Conversation {
	id: string;
	title: string | null;
	created_at: string;
}

export interface Message {
	id: string;
	conversation_id: string;
	role: 'user' | 'assistant' | 'system';
	content: string;
	created_at: string;
}

/** The authenticated user (GET /auth/me). */
export interface Me {
	id: string;
	email: string | null;
	display_name: string | null;
}

export interface Goal {
	id: string;
	user_id: string;
	title: string;
	description: string | null;
	/** Free-text category label (e.g. "Creative", "Learning"), may be null. */
	category: string | null;
	deadline: string | null;
	/** Where the user will work on the goal (free text). */
	location: string | null;
	/** Starting point, e.g. "Beginner" | "Intermediate" | "Advanced". */
	skill_level: string | null;
	/** What "done" looks like, in the user's own words. */
	success_criterion: string | null;
	/** Minutes per practice session. */
	time_per_session_min: number | null;
	/** GoalStatus: draft | active | succeeded | failed | abandoned. */
	status: string;
	progress_note: string | null;
	created_at: string;
	updated_at: string;
}

/** A single logged check-in against a goal (or standalone). */
export interface CheckIn {
	id: string;
	goal_id: string | null;
	user_id: string;
	note: string | null;
	mood: string | null;
	created_at: string;
}

/** Streak summary (GET /api/streak). */
export interface Streak {
	current_streak: number;
	longest_streak: number;
	/** 7 days, oldest -> newest. */
	week: Array<{ date: string; checked: boolean }>;
}

/** A money pledge attached to a goal. */
export interface Pledge {
	id: string;
	goal_id: string;
	user_id: string;
	amount_cents: number;
	/** PledgeStatus: proposed | held | forfeited | refunded. */
	status: string;
	created_at: string;
	resolved_at: string | null;
}

export interface RoadmapStep {
	id: string;
	roadmap_id: string;
	/** Ordering position within the roadmap. */
	ord: number;
	title: string;
	detail: string | null;
	due_date: string | null;
	/** StepStatus: pending | done | skipped. */
	status: string;
	created_at: string;
}

export interface Roadmap {
	id: string;
	goal_id: string;
	model: string | null;
	created_at: string;
	steps: RoadmapStep[];
}

export interface Notification {
	id: string;
	goal_id: string | null;
	kind: string;
	payload: unknown;
	scheduled_at: string;
	sent_at: string | null;
}

/**
 * A suggested circle: a cluster of users pursuing goals similar to one of yours.
 * Derived server-side from goal embeddings; there is no membership/join yet.
 */
export interface CircleSuggestion {
	/** Circle theme, e.g. "Creative", "Learning". */
	category: string;
	/** How many people are working toward something similar. */
	member_count: number;
	/** Mean similarity score of the cluster, 0..1. */
	avg_score: number;
	/** The user's goal this suggestion was matched from. */
	based_on_goal_id: string;
	based_on_goal_title: string;
}

/** GET /api/circles/suggestions payload. */
export interface CircleSuggestions {
	circles: CircleSuggestion[];
}

/** GET /api/goals/:id/similar payload — how many others share a similar goal. */
export interface SimilarGoals {
	similar_user_count: number;
	/** Mean similarity score of the matched users, 0..1. */
	avg_score: number;
}

// --- Typed API helpers --------------------------------------------------------

/**
 * Absolute URL of the backend's OIDC login entry point. Used for the "Sign in"
 * links in the UI (a full-page navigation, not a fetch).
 */
export function loginUrl(): string {
	return `${API_BASE_URL}/auth/login`;
}

/**
 * GET /auth/me — the current user, or throws `ApiError` with status 401 when
 * not signed in. Callers treat 401 as "guest".
 */
export function getMe(options?: RequestOptions): Promise<Me> {
	return request<Me>('GET', '/auth/me', undefined, options);
}

/** GET /api/wallet — current wallet balance. */
export function getWallet(options?: RequestOptions): Promise<Wallet> {
	return request<Wallet>('GET', '/api/wallet', undefined, options);
}

/**
 * POST /api/wallet/topup — top up the wallet.
 * Amount is in cents; the backend enforces min €4 (400) / max €100 (10000).
 */
export function topup(amountCents: number, options?: RequestOptions): Promise<Wallet> {
	return request<Wallet>('POST', '/api/wallet/topup', { amount_cents: amountCents }, options);
}

/** GET /api/conversations — list the user's chat conversations. */
export function listConversations(options?: RequestOptions): Promise<Conversation[]> {
	return request<Conversation[]>('GET', '/api/conversations', undefined, options);
}

/** POST /api/conversations — start a new conversation. */
export function createConversation(
	title?: string,
	options?: RequestOptions
): Promise<Conversation> {
	return request<Conversation>(
		'POST',
		'/api/conversations',
		title !== undefined ? { title } : {},
		options
	);
}

/**
 * POST /api/conversations/:id/messages — send a user message and get the
 * assistant reply.
 *
 * NOTE: the backend contract streams this response as Server-Sent Events
 * (token deltas + a final usage event). For this minimal frontend we treat it
 * as a plain POST that returns the finished assistant message.
 * TODO: switch to SSE streaming (EventSource / fetch + ReadableStream) so
 * replies render token-by-token.
 */
export function sendMessage(
	conversationId: string,
	content: string,
	options?: RequestOptions
): Promise<Message> {
	return request<Message>(
		'POST',
		`/api/conversations/${encodeURIComponent(conversationId)}/messages`,
		{ content },
		options
	);
}

/** Callbacks for {@link streamMessage}. */
export interface StreamHandlers {
	/** A token delta arrived; append it to the in-progress assistant message. */
	onToken: (token: string) => void;
	/** The stream finished; `costCents` is what the wallet was debited. */
	onDone?: (costCents: number) => void;
	/** The backend emitted an inline `error` SSE event mid-stream. */
	onError?: (message: string) => void;
}

/**
 * POST /api/conversations/:id/messages, consuming the Server-Sent Events stream.
 *
 * The backend emits, per the coach contract:
 *   - default (unnamed) events whose `data` is a raw token delta,
 *   - a final `event: done` whose data is `{"cost_cents": <int>}`,
 *   - an optional `event: error` with a plain-text message.
 *
 * A pre-stream failure (e.g. 402 when the wallet is empty) is surfaced by
 * throwing an {@link ApiError} before any tokens are delivered, so callers can
 * inspect `.status === 402`.
 */
export async function streamMessage(
	conversationId: string,
	content: string,
	handlers: StreamHandlers,
	options: RequestOptions = {}
): Promise<void> {
	const url = `${API_BASE_URL}/api/conversations/${encodeURIComponent(conversationId)}/messages`;
	const deviceToken = getDeviceToken();

	let response: Response;
	try {
		response = await fetch(url, {
			method: 'POST',
			credentials: 'include',
			headers: {
				'Content-Type': 'application/json',
				Accept: 'text/event-stream',
				...(deviceToken !== null ? { Authorization: `Bearer ${deviceToken}` } : {}),
				...options.headers
			},
			body: JSON.stringify({ content }),
			signal: options.signal
		});
	} catch (cause) {
		throw new ApiError(
			`Network error while requesting POST ${url}`,
			0,
			cause instanceof Error ? cause.message : cause
		);
	}

	if (!response.ok) {
		const contentType = response.headers.get('content-type') ?? '';
		const isJson = contentType.includes('application/json');
		const payload = isJson ? await response.json().catch(() => undefined) : undefined;
		const message =
			(isJson && payload && typeof payload === 'object' && 'message' in payload
				? String((payload as Record<string, unknown>).message)
				: undefined) ?? `Request failed with status ${response.status}`;
		throw new ApiError(message, response.status, payload);
	}

	if (response.body === null) {
		// No stream body (shouldn't happen for SSE) — nothing to read.
		handlers.onDone?.(0);
		return;
	}

	const reader = response.body.getReader();
	const decoder = new TextDecoder();
	let buffer = '';

	// Parse one SSE frame (already split on the blank-line separator).
	const handleFrame = (frame: string): void => {
		let event = 'message';
		const dataLines: string[] = [];
		for (const rawLine of frame.split('\n')) {
			const line = rawLine.replace(/\r$/, '');
			if (line === '' || line.startsWith(':')) continue;
			const colon = line.indexOf(':');
			const field = colon === -1 ? line : line.slice(0, colon);
			// Strip the field name and exactly one optional leading space (SSE spec).
			let value = colon === -1 ? '' : line.slice(colon + 1);
			if (value.startsWith(' ')) value = value.slice(1);
			if (field === 'event') event = value;
			else if (field === 'data') dataLines.push(value);
		}
		const data = dataLines.join('\n');
		if (event === 'done') {
			let cost = 0;
			try {
				const parsed = JSON.parse(data) as { cost_cents?: number };
				cost = typeof parsed.cost_cents === 'number' ? parsed.cost_cents : 0;
			} catch {
				// ignore malformed done payloads; treat as zero-cost.
			}
			handlers.onDone?.(cost);
		} else if (event === 'error') {
			handlers.onError?.(data);
		} else if (dataLines.length > 0) {
			handlers.onToken(data);
		}
	};

	for (;;) {
		const { done, value } = await reader.read();
		if (done) break;
		buffer += decoder.decode(value, { stream: true });
		let sep: number;
		// Frames are separated by a blank line.
		while ((sep = buffer.indexOf('\n\n')) !== -1) {
			const frame = buffer.slice(0, sep);
			buffer = buffer.slice(sep + 2);
			if (frame.trim() !== '') handleFrame(frame);
		}
	}
	if (buffer.trim() !== '') handleFrame(buffer);
}

/** GET /api/goals — list the user's goals. */
export function listGoals(options?: RequestOptions): Promise<Goal[]> {
	return request<Goal[]>('GET', '/api/goals', undefined, options);
}

/** Optional, snake_case fields shared by create + patch. */
export interface GoalInput {
	title?: string;
	description?: string;
	category?: string;
	deadline?: string;
	location?: string;
	skill_level?: string;
	success_criterion?: string;
	time_per_session_min?: number;
	status?: string;
	progress_note?: string;
}

/** POST /api/goals — create a goal. `title` is required on create. */
export function createGoal(
	input: GoalInput & { title: string },
	options?: RequestOptions
): Promise<Goal> {
	return request<Goal>('POST', '/api/goals', input, options);
}

/** GET /api/goals/:id — fetch a single goal. */
export function getGoal(goalId: string, options?: RequestOptions): Promise<Goal> {
	return request<Goal>('GET', `/api/goals/${encodeURIComponent(goalId)}`, undefined, options);
}

/** PATCH /api/goals/:id — update mutable goal fields. */
export function patchGoal(
	goalId: string,
	input: GoalInput,
	options?: RequestOptions
): Promise<Goal> {
	return request<Goal>('PATCH', `/api/goals/${encodeURIComponent(goalId)}`, input, options);
}

/** PATCH /api/steps/:id — update a roadmap step's status. */
export function patchStep(
	stepId: string,
	status: 'pending' | 'done' | 'skipped',
	options?: RequestOptions
): Promise<RoadmapStep> {
	return request<RoadmapStep>(
		'PATCH',
		`/api/steps/${encodeURIComponent(stepId)}`,
		{ status },
		options
	);
}

// --- Check-ins ----------------------------------------------------------------

/** POST /api/goals/:id/check-ins — log a check-in against a goal. */
export function createGoalCheckIn(
	goalId: string,
	input: { note?: string; mood?: string },
	options?: RequestOptions
): Promise<CheckIn> {
	return request<CheckIn>(
		'POST',
		`/api/goals/${encodeURIComponent(goalId)}/check-ins`,
		input,
		options
	);
}

/** GET /api/goals/:id/check-ins — a goal's check-ins. */
export function listGoalCheckIns(goalId: string, options?: RequestOptions): Promise<CheckIn[]> {
	return request<CheckIn[]>(
		'GET',
		`/api/goals/${encodeURIComponent(goalId)}/check-ins`,
		undefined,
		options
	);
}

/** POST /api/check-ins — log a standalone (or goal-linked) check-in. */
export function createCheckIn(
	input: { note?: string; mood?: string; goal_id?: string },
	options?: RequestOptions
): Promise<CheckIn> {
	return request<CheckIn>('POST', '/api/check-ins', input, options);
}

/** GET /api/check-ins — all of the user's check-ins. */
export function listCheckIns(options?: RequestOptions): Promise<CheckIn[]> {
	return request<CheckIn[]>('GET', '/api/check-ins', undefined, options);
}

// --- Streak -------------------------------------------------------------------

/** GET /api/streak — current/longest streak + a 7-day (oldest->newest) window. */
export function getStreak(options?: RequestOptions): Promise<Streak> {
	return request<Streak>('GET', '/api/streak', undefined, options);
}

// --- Pledges ------------------------------------------------------------------

/**
 * GET /api/goals/:id/pledge — the goal's pledge, or `null` when there is none
 * (the backend returns 204 No Content in that case).
 */
export async function getPledge(goalId: string, options?: RequestOptions): Promise<Pledge | null> {
	try {
		const pledge = await request<Pledge | undefined>(
			'GET',
			`/api/goals/${encodeURIComponent(goalId)}/pledge`,
			undefined,
			options
		);
		// 204 responses have no JSON body -> request() resolves to undefined.
		return pledge ?? null;
	} catch (err) {
		if (err instanceof ApiError && err.status === 404) return null;
		throw err;
	}
}

/** POST /api/goals/:id/pledge — propose a pledge of `amountCents`. */
export function createPledge(
	goalId: string,
	amountCents: number,
	options?: RequestOptions
): Promise<Pledge> {
	return request<Pledge>(
		'POST',
		`/api/goals/${encodeURIComponent(goalId)}/pledge`,
		{ amount_cents: amountCents },
		options
	);
}

/** POST /api/goals/:id/pledge/confirm — place the hold on the proposed pledge. */
export function confirmPledge(goalId: string, options?: RequestOptions): Promise<Pledge> {
	return request<Pledge>(
		'POST',
		`/api/goals/${encodeURIComponent(goalId)}/pledge/confirm`,
		{},
		options
	);
}

/** POST /api/goals/:id/roadmap — generate a roadmap for a goal (LLM). */
export function generateRoadmap(goalId: string, options?: RequestOptions): Promise<Roadmap> {
	return request<Roadmap>('POST', `/api/goals/${encodeURIComponent(goalId)}/roadmap`, {}, options);
}

/** GET /api/goals/:id/roadmap — fetch an existing roadmap. */
export function getRoadmap(goalId: string, options?: RequestOptions): Promise<Roadmap> {
	return request<Roadmap>(
		'GET',
		`/api/goals/${encodeURIComponent(goalId)}/roadmap`,
		undefined,
		options
	);
}

/** GET /api/notifications — in-app notifications (reminders, tips, check-ins). */
export function listNotifications(options?: RequestOptions): Promise<Notification[]> {
	return request<Notification[]>('GET', '/api/notifications', undefined, options);
}

// --- Circles ------------------------------------------------------------------

/**
 * GET /api/circles/suggestions — clusters of users pursuing goals similar to
 * the caller's. Empty (`{ circles: [] }`) is the normal early case when few
 * users have embedded goals; callers show an honest empty state, never fakes.
 */
export function getCircleSuggestions(options?: RequestOptions): Promise<CircleSuggestions> {
	return request<CircleSuggestions>('GET', '/api/circles/suggestions', undefined, options);
}

/**
 * GET /api/goals/:id/similar — how many other users have a similar goal.
 * Requires goal embeddings, so `similar_user_count: 0` is the honest normal
 * case until an OpenRouter key is configured; callers hide the line when 0.
 */
export function getSimilar(goalId: string, options?: RequestOptions): Promise<SimilarGoals> {
	return request<SimilarGoals>(
		'GET',
		`/api/goals/${encodeURIComponent(goalId)}/similar`,
		undefined,
		options
	);
}
