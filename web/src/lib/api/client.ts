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

export interface Goal {
	id: string;
	title: string;
	description: string | null;
	deadline: string | null;
	status: string;
	created_at: string;
}

export interface RoadmapStep {
	id: string;
	roadmap_id: string;
	title: string;
	description: string | null;
	due_at: string | null;
	done: boolean;
	position: number;
}

export interface Roadmap {
	id: string;
	goal_id: string;
	steps: RoadmapStep[];
	created_at: string;
}

export interface Notification {
	id: string;
	goal_id: string | null;
	kind: string;
	payload: unknown;
	scheduled_at: string;
	sent_at: string | null;
}

// --- Typed API helpers --------------------------------------------------------

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

/** GET /api/goals — list the user's goals. */
export function listGoals(options?: RequestOptions): Promise<Goal[]> {
	return request<Goal[]>('GET', '/api/goals', undefined, options);
}

/** POST /api/goals — create a goal. */
export function createGoal(
	input: { title: string; description?: string; deadline?: string },
	options?: RequestOptions
): Promise<Goal> {
	return request<Goal>('POST', '/api/goals', input, options);
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
