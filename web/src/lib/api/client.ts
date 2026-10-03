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
	/** True when a photo is attached (fetch it from GET /api/check-ins/:id/photo). */
	has_photo: boolean;
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
	/** This step's own effort estimate, e.g. "4-5 h ride" (free text), may be null. */
	effort: string | null;
	due_date: string | null;
	/** StepStatus: pending | done | skipped. */
	status: string;
	created_at: string;
}

/**
 * A single checkable TODO item under a roadmap step (milestone). Generated by
 * the coach (LLM) on demand and ticked off as the user does them.
 */
export interface Todo {
	id: string;
	/** The roadmap step (milestone) this TODO belongs to. */
	step_id: string;
	/** Ordering position within the step's TODO list. */
	ord: number;
	title: string;
	done: boolean;
	/**
	 * What kind of to-do this is: 'training' lives under a milestone, while
	 * 'gear' | 'logistics' | 'prep' | 'other' are goal-level (gear/prep/logistics)
	 * shown in the goal's "Gear & logistics" section.
	 */
	category: string;
	created_at: string;
	/** When it was ticked off, or null while still open. */
	done_at: string | null;
}

/**
 * A planned away-period on the roadmap (e.g. Christmas, a holiday) that the
 * timeline renders as a distinct "break" block between milestones.
 */
export interface Break {
	id: string;
	label: string;
	/** YYYY-MM-DD. */
	start_date: string;
	/** YYYY-MM-DD. */
	end_date: string;
}

export interface Roadmap {
	id: string;
	goal_id: string;
	model: string | null;
	created_at: string;
	steps: RoadmapStep[];
	/** Planned breaks (Christmas, vacations, …); empty when there are none. */
	breaks: Break[];
}

export interface Notification {
	id: string;
	goal_id: string | null;
	/** The roadmap step this notification relates to, when any. */
	step_id: string | null;
	kind: string;
	payload: Record<string, unknown>;
	scheduled_at: string;
	sent_at: string | null;
	/** Delivery channel (e.g. "in_app", "push"). */
	channel: string;
	created_at: string;
	/** When the caller marked this read, or null while still unread. */
	read_at: string | null;
}

/**
 * The coach's structured review of a check-in. Delivered both inline on the
 * check-in response and as a `kind: "coach"` notification (in its `payload`).
 */
export interface CoachReview {
	status: 'on_track' | 'ahead' | 'at_risk' | 'off_track';
	headline: string;
	message: string;
	questions: string[];
	suggestions: string[];
	risks: string[];
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

/** A model the coach may be configured to use (from GET /api/settings). */
export interface ModelOption {
	id: string;
	label: string;
}

/** User-facing settings (GET /api/settings). */
export interface Settings {
	/** The model id currently used for chat + roadmaps. */
	chat_model: string;
	/** The models the backend allows; only these should be offered in the UI. */
	allowed_models: ModelOption[];
	/**
	 * The user's country as an ISO-3166-1 alpha-2 code (e.g. "DE"), or null when
	 * unset. Used for holiday-aware planning.
	 */
	country: string | null;
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

/** GET /api/conversations/:id payload — a conversation plus its messages. */
export interface ConversationDetail {
	conversation: Conversation;
	messages: Message[];
}

/** GET /api/conversations/:id — fetch a conversation with its full message history. */
export function getConversation(
	conversationId: string,
	options?: RequestOptions
): Promise<ConversationDetail> {
	return request<ConversationDetail>(
		'GET',
		`/api/conversations/${encodeURIComponent(conversationId)}`,
		undefined,
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

/** The reply from the agentic coach turn (POST /api/conversations/:id/agent). */
export interface AgentReply {
	/** The coach's assistant message for this turn. */
	reply: string;
	/** Set to the new goal's id when the coach created a goal during the turn. */
	created_goal_id: string | null;
}

/**
 * POST /api/conversations/:id/agent — run one agentic coach turn.
 *
 * The backend runs the coach agent: it may ask follow-ups, propose a plan, and
 * (via a server-side tool) create the goal once the user has confirmed in chat.
 * This is a plain, NON-streaming request — the turn can take a few seconds. The
 * user + assistant messages are persisted server-side.
 *
 * Can throw {@link ApiError} with `.status === 402` (wallet empty) or
 * `.status === 400` (no LLM key configured); callers surface those gently.
 */
export function sendAgentMessage(
	conversationId: string,
	content: string,
	options?: RequestOptions
): Promise<AgentReply> {
	return request<AgentReply>(
		'POST',
		`/api/conversations/${encodeURIComponent(conversationId)}/agent`,
		{ content },
		options
	);
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

// --- Milestone TODOs ----------------------------------------------------------

/**
 * GET /api/steps/:id/todos — the step's TODO items, in order. Resolves to an
 * empty array when none have been generated yet.
 */
export function getStepTodos(stepId: string, options?: RequestOptions): Promise<Todo[]> {
	return request<Todo[]>(
		'GET',
		`/api/steps/${encodeURIComponent(stepId)}/todos`,
		undefined,
		options
	);
}

/**
 * GET /api/goals/:id/todos — the goal's NON-training to-dos (gear / logistics /
 * prep / other), in order. These are added by the coach from check-ins and shown
 * in the goal's "Gear & logistics" section. Resolves to an empty array when none.
 */
export function getGoalTodos(goalId: string, options?: RequestOptions): Promise<Todo[]> {
	return request<Todo[]>(
		'GET',
		`/api/goals/${encodeURIComponent(goalId)}/todos`,
		undefined,
		options
	);
}

/**
 * POST /api/steps/:id/todos/generate — ask the coach (LLM) to generate this
 * step's TODO list. Idempotent: returns the existing list if already generated.
 *
 * Can throw {@link ApiError} with `.status === 402` (wallet empty); callers
 * detect PaymentRequired and surface a top-up affordance instead of retrying.
 */
export function generateStepTodos(stepId: string, options?: RequestOptions): Promise<Todo[]> {
	return request<Todo[]>(
		'POST',
		`/api/steps/${encodeURIComponent(stepId)}/todos/generate`,
		{},
		options
	);
}

/** PATCH /api/todos/:id — mark a TODO done (or not). Returns the updated TODO. */
export function toggleTodo(todoId: string, done: boolean, options?: RequestOptions): Promise<Todo> {
	return request<Todo>('PATCH', `/api/todos/${encodeURIComponent(todoId)}`, { done }, options);
}

// --- Check-ins ----------------------------------------------------------------

/**
 * The response from logging a goal check-in. It is a {@link CheckIn} plus a
 * coach `suggestion`: when `adjust` is true the coach is offering to tweak the
 * plan (with a `message` to show), otherwise it's a plain recorded check-in.
 */
export interface GoalCheckInResponse extends CheckIn {
	suggestion: { adjust: boolean; message: string | null };
	/** TODOs auto-ticked from this check-in's note (empty when none matched). */
	todos_completed: Todo[];
	/**
	 * New to-dos the coach added from this check-in's note (e.g. gear/logistics the
	 * note surfaced). Can be training (milestone) or goal-level; empty when none.
	 */
	todos_added: Todo[];
	/**
	 * The coach's structured review of this check-in, when the coach weighed in
	 * (null when there's no review — e.g. coaching unavailable). Additive: the
	 * existing `suggestion.adjust` flow is unchanged.
	 */
	coach: CoachReview | null;
}

/** POST /api/goals/:id/check-ins — log a check-in against a goal. */
export function createGoalCheckIn(
	goalId: string,
	input: { note?: string; mood?: string },
	options?: RequestOptions
): Promise<GoalCheckInResponse> {
	return request<GoalCheckInResponse>(
		'POST',
		`/api/goals/${encodeURIComponent(goalId)}/check-ins`,
		input,
		options
	);
}

/**
 * A structured, presentational preview of the changes the coach is proposing in
 * an {@link AdjustReply}. Purely for display — the existing "reply 'yes' to
 * apply" confirm flow is what actually applies them.
 */
export interface AdjustProposal {
	/** One-line human summary of the proposed change. */
	summary: string;
	/** Breaks the coach would add (dates are YYYY-MM-DD). */
	add_breaks: { label: string; start_date: string; end_date: string }[];
	/** Human labels of breaks the coach would remove. */
	remove_breaks: string[];
	/** Milestone date moves; `old_due`/`new_due` are YYYY-MM-DD or null. */
	milestone_changes: { title: string; old_due: string | null; new_due: string | null }[];
}

/** The reply from an "adjust my plan" coach turn (POST /api/goals/:id/adjust). */
export interface AdjustReply {
	/** The coach's assistant message for this turn. */
	reply: string;
	/** True only when the coach actually altered the plan this turn. */
	changed: boolean;
	/** A structured preview of the proposed change, when the coach has one. */
	proposal?: AdjustProposal | null;
}

/**
 * POST /api/goals/:id/adjust — one agentic, NON-streaming coach turn that can
 * change the plan's timeline/breaks (propose-then-confirm). The turn may take a
 * few seconds; `changed` is true only when the coach actually altered the plan,
 * in which case callers should re-fetch the roadmap. When the coach is proposing
 * (not yet applying) a change, `proposal` carries a structured preview.
 *
 * Can throw {@link ApiError} with `.status === 402` (wallet empty) or
 * `.status === 400` (coach unavailable); callers surface those gently.
 */
export function adjustPlan(
	goalId: string,
	content: string,
	options?: RequestOptions
): Promise<AdjustReply> {
	return request<AdjustReply>(
		'POST',
		`/api/goals/${encodeURIComponent(goalId)}/adjust`,
		{ content },
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

/**
 * POST /api/check-ins/:id/photo — attach a photo to a check-in.
 *
 * This is NOT a JSON endpoint: the body is the RAW image bytes and the
 * `Content-Type` is the image's own mime. It bypasses the JSON {@link request}
 * helper but keeps the same auth (session cookie + device-token bearer). The
 * backend replies 204 No Content on success. Throws {@link ApiError} otherwise.
 */
export async function uploadCheckInPhoto(
	checkInId: string,
	image: Blob,
	options: RequestOptions = {}
): Promise<void> {
	const url = `${API_BASE_URL}/api/check-ins/${encodeURIComponent(checkInId)}/photo`;
	const deviceToken = getDeviceToken();
	let response: Response;
	try {
		response = await fetch(url, {
			method: 'POST',
			credentials: 'include',
			headers: {
				'Content-Type': image.type || 'application/octet-stream',
				...(deviceToken !== null ? { Authorization: `Bearer ${deviceToken}` } : {}),
				...options.headers
			},
			body: image,
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
		throw new ApiError(`Request failed with status ${response.status}`, response.status);
	}
}

/**
 * GET /api/check-ins/:id/photo — fetch a check-in's photo as a Blob.
 *
 * Authenticated (cookie + device-token bearer), so an `<img src>` pointed
 * straight at this path wouldn't carry the bearer in the app build. Callers
 * fetch the bytes here and wrap them in an object URL instead. Returns `null`
 * when there's no photo (404); throws {@link ApiError} on other failures.
 */
export async function fetchCheckInPhoto(
	checkInId: string,
	options: RequestOptions = {}
): Promise<Blob | null> {
	const url = `${API_BASE_URL}/api/check-ins/${encodeURIComponent(checkInId)}/photo`;
	const deviceToken = getDeviceToken();
	let response: Response;
	try {
		response = await fetch(url, {
			method: 'GET',
			credentials: 'include',
			headers: {
				...(deviceToken !== null ? { Authorization: `Bearer ${deviceToken}` } : {}),
				...options.headers
			},
			signal: options.signal
		});
	} catch (cause) {
		throw new ApiError(
			`Network error while requesting GET ${url}`,
			0,
			cause instanceof Error ? cause.message : cause
		);
	}
	if (!response.ok) {
		if (response.status === 404) return null;
		throw new ApiError(`Request failed with status ${response.status}`, response.status);
	}
	return response.blob();
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

/** PATCH /api/notifications/:id/read — mark one notification read (204). */
export async function markNotificationRead(id: string, options?: RequestOptions): Promise<void> {
	await request<void>('PATCH', `/api/notifications/${encodeURIComponent(id)}/read`, {}, options);
}

/** POST /api/notifications/read-all — mark all of the caller's notifications read (204). */
export async function markAllNotificationsRead(options?: RequestOptions): Promise<void> {
	await request<void>('POST', '/api/notifications/read-all', {}, options);
}

/**
 * POST /api/push-tokens — register (upsert) this device's push token so the
 * backend can deliver push notifications to it. `platform` is the device
 * platform ("android" | "ios" | "web") and `token` the platform push token
 * (FCM token on Android). Idempotent server-side (upsert). App build only:
 * see `$lib/app/pushRegister`, which only calls this inside the Tauri runtime.
 */
export async function upsertPushToken(
	platform: string,
	token: string,
	options?: RequestOptions
): Promise<void> {
	await request<void>('POST', '/api/push-tokens', { platform, token }, options);
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

// --- Settings -----------------------------------------------------------------

/** GET /api/settings — the current chat model plus the allowed choices. */
export function getSettings(options?: RequestOptions): Promise<Settings> {
	return request<Settings>('GET', '/api/settings', undefined, options);
}

/**
 * PUT /api/settings — change the chat model and country. Returns the saved
 * values. `country` is an ISO-3166-1 alpha-2 code, or null to clear it.
 * The backend responds 400 (ApiError.status === 400) if `chatModel` is not one
 * of the allowed models.
 */
export function updateSettings(
	chatModel: string,
	country: string | null,
	options?: RequestOptions
): Promise<{ chat_model: string; country: string | null }> {
	return request<{ chat_model: string; country: string | null }>(
		'PUT',
		'/api/settings',
		{ chat_model: chatModel, country },
		options
	);
}

/**
 * GET /api/settings/suggested-country — an offline, IP-based best guess at the
 * user's country as an ISO-3166-1 alpha-2 code. Returns null when the backend
 * can't guess. Used to pre-fill (not auto-save) the country selector.
 */
export async function getSuggestedCountry(options?: RequestOptions): Promise<string | null> {
	const result = await request<{ country: string | null }>(
		'GET',
		'/api/settings/suggested-country',
		undefined,
		options
	);
	return result.country;
}
