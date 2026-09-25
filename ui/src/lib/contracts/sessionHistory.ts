/** Session history and resume command DTOs in their Rust/Tauri wire shape. */

import type { RequestKind } from '../modelRoles.ts';

/** Persisted session row returned by the history commands. */
export interface SessionHistoryRow {
	id: string;
	input_text: string;
	title: string | null;
	status: string;
	created_at: string;
	updated_at: string;
}

/** A live session summary returned by `get_sessions`. */
export interface SessionInfoStep {
	id: string;
	step_number: number;
	tool_name: string;
	input: unknown;
	output: unknown | null;
	status: string;
	risk_level: string;
	confirmed: boolean | null;
}

export interface SessionInfo {
	id: string;
	input: string;
	summary: string;
	title: string | null;
	status: string;
	waiting_reason?: string;
	steps: SessionInfoStep[];
	created_at: string;
	updated_at: string;
}

export interface SessionListResponse {
	sessions: SessionInfo[];
}

export interface SessionMessageAttachment {
	media_type: string;
	data: string;
	asset_id?: string;
	filename?: string;
	path?: string;
	sha256?: string;
	size_bytes?: number;
	expires_at?: string;
	representations?: unknown[];
	preferred_representation?: string;
}

/** Rust `Message` fields returned by a session resume command. */
export interface SessionResumeMessage {
	id: string;
	session_id: string;
	role: string;
	content: string;
	message_type: string | null;
	created_at: string;
	tool_call_id: string | null;
	attachments: SessionMessageAttachment[];
	media_inputs: unknown[];
	voice: boolean;
}

/** Rust `SessionStep` fields returned by a session resume command. */
export interface SessionResumeStep {
	id: string;
	session_id: string;
	step_number: number;
	action_index: number;
	thought: string | null;
	action_tool: string | null;
	action_input: string | null;
	tool_call_id: string | null;
	observation: string | null;
	status: string;
	is_high_risk: boolean;
	confirmed: boolean | null;
	silent: boolean;
	started_at: string | null;
	completed_at: string | null;
	created_at: string;
}

/** Persisted cumulative usage row returned by a resume command. */
export interface SessionResumeUsage {
	prompt_tokens: number;
	completion_tokens: number;
	total_tokens: number;
	cached_tokens: number;
	cache_creation_tokens: number;
	cache_miss_tokens: number;
	context_tokens: number;
	context_window: number | null;
	cost_usd: number;
	has_cost: boolean;
}

/** Shared renderer usage row for persisted resume data and live LLM events. */
export interface SessionLlmUsage {
	id?: string;
	step_number?: number | null;
	role?: RequestKind;
	call_kind: string;
	model?: string | null;
	prompt_tokens?: number;
	completion_tokens?: number;
	total_tokens?: number;
	cached_tokens?: number;
	cache_creation_tokens?: number;
	cache_miss_tokens?: number;
	cache_accounting?: string;
	cache_diagnostics?: {
		mode?: string;
		key_requested?: boolean;
		system_split?: boolean;
		downgraded?: boolean;
		outcome?: string;
	};
	context_tokens?: number;
	context_window?: number | null;
	cost_usd?: number | null;
	has_cost?: boolean;
	duration_ms?: number | null;
	created_at?: string;
}

/** Renderer-safe interaction projection included in a resume response. */
export interface SessionResumeInteraction {
	id: string;
	session_id: string;
	kind: string;
	status: string;
	prompt: string;
	options?: string[];
	tool_name?: string;
	risk_level?: string;
	summary?: string;
	permission_key?: string;
	invocation_step_id?: string;
	action_index?: number;
	tool_call_id?: string;
	created_at: string;
	expires_at?: string;
}

/** Complete successful response from `get_session_for_resume`/`get_last_conversation`. */
export interface SessionResumeResponse {
	session: SessionHistoryRow;
	messages: SessionResumeMessage[];
	steps: SessionResumeStep[];
	usage: SessionResumeUsage | null;
	llm_usage: SessionLlmUsage[];
	interactions: SessionResumeInteraction[];
}

type ResumeMessageInput = Pick<SessionResumeMessage, 'id' | 'role' | 'content' | 'created_at'> &
	Partial<Omit<SessionResumeMessage, 'id' | 'role' | 'content' | 'created_at'>>;
type ResumeStepInput = Pick<SessionResumeStep, 'id' | 'step_number' | 'created_at'> &
	Partial<Omit<SessionResumeStep, 'id' | 'step_number' | 'created_at'>>;

/** Tolerant projection input accepted by the pure resume-message builder. */
export interface SessionResumeInput {
	session?: Partial<SessionHistoryRow> | null;
	messages?: ResumeMessageInput[];
	steps?: ResumeStepInput[];
	usage?: Partial<SessionResumeUsage> | null;
	llm_usage?: SessionLlmUsage[];
	interactions?: unknown[];
}

export type SessionResumeMessageInput = ResumeMessageInput;
export type SessionResumeStepInput = ResumeStepInput;
