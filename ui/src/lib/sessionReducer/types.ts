import type {
	AgentActionPayload,
	AgentChunkPayload,
	AgentObservationPayload,
	AgentStreamResetPayload,
	AgentSupplementPayload,
	AgentThoughtPayload,
	AgentWebSearchPayload,
} from '../contracts/agent.ts';
import type { InteractionKind, InteractionRequest } from '../contracts/app.ts';
import type { SessionResumeUsage } from '../contracts/sessionHistory.ts';
import type { StreamMessage } from '../streaming.ts';
import type { LlmUsage } from '../sessionUsage.ts';

export const DRAFT_SESSION_ID = '_draft';

/** The session summary fields used by the chat shell. */
export interface SessionSummary {
	id: string;
	status: string;
	waitingReason?: unknown;
	[key: string]: unknown;
}

export interface SessionError {
	sessionId: string;
	reason: string;
}

export type SessionTerminationStatus = 'paused' | 'completed' | 'error';

export interface SessionTermination {
	sessionId: string;
	status: SessionTerminationStatus;
	reason: string;
}

/** One renderer message, including the explicit dynamic tool extension points. */
export type SessionMessage = StreamMessage & {
	attachments?: Array<{ media_type: string; data: string; filename?: string }>;
	toolArgs?: unknown;
	outcome?: string | null;
	renderer?: string | null;
	actionId?: string | null;
	/** Stable Action identity used to anchor its timeline card to this tool step. */
	sourceActionId?: string | null;
	resolved?: { answer?: string; ignored?: boolean } | null;
	received?: boolean;
};

export interface SessionTokenStats {
	promptTokens: number;
	completionTokens: number;
	totalTokens: number;
	cachedTokens?: number;
	cacheCreationTokens?: number;
	cacheMissTokens?: number;
	contextTokens?: number;
	cacheExclusive?: boolean;
	cumulativePromptTokens: number;
	cumulativeCompletionTokens: number;
	cumulativeTotalTokens: number;
	cumulativeCachedTokens?: number;
	cumulativeCacheCreationTokens?: number;
	cumulativeCacheMissTokens?: number;
	costUsd: number | null;
	cumulativeCostUsd: number | null;
	contextWindow: number | null;
	model: string | null;
	cacheAccounting?: string;
	cacheDiagnostics?: unknown;
	restored?: boolean;
	lastUpdated?: number;
}

export interface SessionReplayState {
	eventSeqBySession: Record<string, string[]>;
	chunkSeqByMessage: Record<string, number>;
	blockIdsBySession: Record<string, Record<string, StreamBlockIds>>;
}

export interface SessionOptimisticMessage {
	sessionId: string;
	messageId: string;
	status: 'pending' | 'accepted' | 'rejected';
}

export interface AgentChunkBatchItem {
	kind: 'thought' | 'reasoning';
	msgType?: string;
	payload: AgentChunkPayload;
}

/** One in-memory runtime state tree for the conversation. */
export interface SessionReducerState {
	sessions: SessionSummary[];
	activeSessionId: string | null;
	error: SessionError | null;
	termination: SessionTermination | null;
	/** Volatile per-session error reasons used when reopening history during this app run. */
	sessionErrorReasons: Record<string, string>;
	messages: Record<string, SessionMessage[]>;
	interactions: Record<string, InteractionRequest>;
	tokenStats: Record<string, SessionTokenStats>;
	llmUsage: Record<string, LlmUsage[]>;
	replay: SessionReplayState;
	optimistic: Record<string, SessionOptimisticMessage>;
}

export type ResumeUsage = Partial<SessionResumeUsage>;

export type SessionAction =
	| { type: 'sessions/loaded'; sessions: SessionSummary[]; autoSelect?: boolean }
	| { type: 'sessions/cleared' }
	| {
			type: 'session/created';
			sessionId: string;
			freshStart: boolean;
			adoptedDraft: boolean;
			status?: string;
			title?: string | null;
	  }
	| { type: 'session/selected'; sessionId: string | null }
	| { type: 'session/cleared' }
	| { type: 'session/deleted'; sessionId: string | null }
	| {
			type: 'session/status-updated';
			sessionId: string;
			status: string;
			title?: string | null;
			waitingReason?: string | null;
	  }
	| { type: 'session/error-shown'; sessionId: string; reason: string }
	| { type: 'session/error-cleared'; sessionId?: string | null }
	| { type: 'session/error-reason-remembered'; sessionId: string; reason: string }
	| { type: 'session/error-reason-forgotten'; sessionId: string }
	| {
			type: 'session/termination-shown';
			sessionId: string;
			status: SessionTerminationStatus;
			reason: string;
	  }
	| { type: 'session/retained-error'; session: SessionSummary }
	| { type: 'session/title-updated'; sessionId: string; title: string }
	| { type: 'session/messages/optimistic-added'; sessionId: string; message: SessionMessage }
	| {
			type: 'session/messages/accepted';
			fromSessionId: string;
			toSessionId: string;
			optimisticId: string;
			persistedId?: string | null;
	  }
	| { type: 'session/messages/rejected'; sessionId: string; messageId: string }
	| { type: 'session/messages/cleared'; sessionId: string }
	| { type: 'session/messages/finalized'; sessionId: string }
	| { type: 'session/messages/adopt-draft'; sessionId: string }
	| {
			type: 'session/messages/asks-settled';
			sessionId: string;
			asks: Array<{
				id: string;
				resolved?: { answer?: string; ignored?: boolean } | null;
			}>;
	  }
	| {
			type: 'session/messages/resume-loaded';
			sessionId: string;
			messages: SessionMessage[];
			interactions?: InteractionRequest[];
			/** Pending live requests that must survive a possibly stale resume snapshot. */
			preserveInteractionIds?: string[];
			usage?: ResumeUsage | null;
			llmUsage?: LlmUsage[];
			preserveStreamingOnly?: boolean;
			excludeMessageIds?: string[];
	  }
	| { type: 'session/messages/truncated'; sessionId: string; targetStep: number }
	| { type: 'session/memory-cleared'; sessionId: string }
	| { type: 'session/replay-reset'; sessionId: string }
	| { type: 'session/stream-blocks-cleared'; sessionId: string }
	| { type: 'session/background-result'; sessionId?: string; actionId: string; content: string }
	| { type: 'session/interaction-upserted'; request: InteractionRequest }
	| {
			type: 'session/interactions-hydrated';
			sessionId: string;
			requests: InteractionRequest[];
			preserveInteractionIds?: string[];
	  }
	| { type: 'session/interactions-cleared'; sessionId: string; kind?: InteractionKind }
	| { type: 'session/interaction-resolved'; id: string; response?: unknown }
	| { type: 'agent/chunks'; chunks: AgentChunkBatchItem[] }
	| { type: 'agent/thought'; payload: AgentThoughtPayload }
	| { type: 'agent/stream-reset'; payload: AgentStreamResetPayload }
	| { type: 'agent/web-search'; payload: AgentWebSearchPayload }
	| { type: 'agent/supplement'; payload: AgentSupplementPayload }
	| { type: 'agent/action'; payload: AgentActionPayload }
	| { type: 'agent/observation'; payload: AgentObservationPayload }
	| {
			type: 'session/usage-restored';
			sessionId: string;
			usage: ResumeUsage | null | undefined;
			llmUsage?: LlmUsage[];
	  }
	| { type: 'session/usage-live'; sessionId: string; stats?: SessionTokenStats; call?: LlmUsage }
	| { type: 'session/usage-cleared'; sessionId: string };

export interface StreamBlockIds {
	thoughtId?: string;
	reasoningId?: string;
}

export const initialSessionState: SessionReducerState = {
	sessions: [],
	activeSessionId: null,
	error: null,
	termination: null,
	sessionErrorReasons: {},
	messages: {},
	interactions: {},
	tokenStats: {},
	llmUsage: {},
	replay: { eventSeqBySession: {}, chunkSeqByMessage: {}, blockIdsBySession: {} },
	optimistic: {},
};

export type SessionActionOf<Type extends SessionAction['type']> = Extract<
	SessionAction,
	{ type: Type }
>;
