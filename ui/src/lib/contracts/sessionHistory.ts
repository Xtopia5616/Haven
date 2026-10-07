/** Session history and resume command DTOs in their Rust/Tauri wire shape. */
import type {
  LlmUsageRecord as GeneratedLlmUsageRecord,
  Message as GeneratedMessage,
  RuntimeSessionListResponse as GeneratedSessionListResponse,
  SessionResumeResponse as GeneratedSessionResumeResponse,
  SessionStep as GeneratedSessionStep,
  SessionUsage as GeneratedSessionUsage,
  TauriCommandResponse,
} from './generatedCommands.ts';

/** All command wire shapes are derived from Rust handlers and DTOs. */
export type SessionHistoryRow = TauriCommandResponse<'list_session_history'>[number];
export type RuntimeSessionListResponse = GeneratedSessionListResponse;
export type SessionLineageResponse = TauriCommandResponse<'get_session_lineage'>;
export type SessionResumeMessage = GeneratedMessage;
export type SessionResumeStep = GeneratedSessionStep;
export type SessionResumeUsage = GeneratedSessionUsage;

/** Renderer usage projection derives its fields from the Rust usage DTO. */
export type SessionLlmUsage = Omit<
	Partial<GeneratedLlmUsageRecord>,
	'cache_diagnostics' | 'cost_usd'
> & {
	call_kind: string;
	cache_diagnostics?: unknown;
	cost_usd?: number | null;
};

/** Complete successful response from get_session_for_resume/get_latest_session_for_resume. */
export type SessionResumeResponse = GeneratedSessionResumeResponse;

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
