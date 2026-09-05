/** Agent event IPC contract at the frontend boundary. */

import type { TauriEvent } from './session.ts';

export const AGENT_EVENT_NAMES = [
	'agent:thought',
	'agent:action',
	'agent:observation',
	'agent:thought_chunk',
	'agent:reasoning_chunk',
	'agent:stream_reset',
	'agent:web_search',
	'agent:stream_stalled',
	'agent:supplement',
	'agent:compaction',
	'agent:usage',
	'agent:tool_output',
	'notification:show',
] as const;

export type AgentEventName = (typeof AGENT_EVENT_NAMES)[number];

export interface AgentThoughtPayload {
	sessionId: string;
	thought: string;
	stepNumber: number;
	runId: number;
	messageId: string;
}

export interface AgentActionPayload {
	sessionId: string;
	toolName: string;
	input: unknown;
	stepNumber: number;
	runId: number;
	toolCallId: string | null;
	actionIndex: number;
	stepId: string;
	suppressStreamedThought: boolean;
	silent: boolean;
}

export interface AgentObservationPayload {
	sessionId: string;
	observation: string;
	toolName: string;
	stepNumber: number;
	runId: number;
	silent: boolean;
	toolCallId: string | null;
	actionIndex: number;
	askOptions: string[];
	stepId: string;
}

export interface AgentChunkPayload {
	sessionId: string;
	delta: string;
	stepNumber: number;
	runId: number;
	messageId: string;
	seq: number;
}

export interface AgentStreamResetPayload {
	sessionId: string;
	stepNumber: number;
	runId: number;
	thoughtMessageId: string;
	reasoningMessageId: string;
}

export interface AgentWebSearchPayload {
	sessionId: string;
	phase: string;
	stepNumber: number;
	runId: number;
	callId?: string;
	action?: string;
	result?: unknown;
}

export interface AgentStreamStalledPayload {
	sessionId: string;
}

export interface AgentSupplementPayload {
	sessionId: string;
	additionalContext: string;
	stepNumber: number;
	runId: number;
	injectSource?: string;
}

export interface AgentCompactionPayload {
	sessionId: string;
	summary: string;
	tokensBefore: number;
	tokensAfter: number;
	degraded: boolean;
	episodeId?: string;
}

export interface AgentNotificationPayload {
	sessionId: string;
	title: string;
	body: string;
}

export interface AgentUsagePayload {
	sessionId: string;
	promptTokens: number;
	completionTokens: number;
	totalTokens: number;
	cachedTokens: number;
	cacheCreationTokens: number;
	cacheMissTokens: number;
	contextTokens: number;
	cacheExclusive: boolean;
	cacheAccounting: string;
	costUsd: number | null;
	model: string | null;
	cumulativePromptTokens: number;
	cumulativeCompletionTokens: number;
	cumulativeTotalTokens: number;
	cumulativeCachedTokens: number;
	cumulativeCacheCreationTokens: number;
	cumulativeCacheMissTokens: number;
	cacheDiagnostics?: unknown;
	cumulativeCostUsd: number | null;
	contextWindow: number | null;
	stepNumber?: number;
	durationMs?: number;
	role?: string;
	hasCost: boolean;
}

export interface AgentToolOutputPayload {
	sessionId: string;
	stepId: string;
	output: string;
}

export interface AgentEventPayloadMap {
	'agent:thought': AgentThoughtPayload;
	'agent:action': AgentActionPayload;
	'agent:observation': AgentObservationPayload;
	'agent:thought_chunk': AgentChunkPayload;
	'agent:reasoning_chunk': AgentChunkPayload;
	'agent:stream_reset': AgentStreamResetPayload;
	'agent:web_search': AgentWebSearchPayload;
	'agent:stream_stalled': AgentStreamStalledPayload;
	'agent:supplement': AgentSupplementPayload;
	'agent:compaction': AgentCompactionPayload;
	'agent:usage': AgentUsagePayload;
	'agent:tool_output': AgentToolOutputPayload;
	'notification:show': AgentNotificationPayload;
}

interface AgentThoughtWirePayload {
	session_id: string;
	thought: string;
	step_number: number;
	run_id: number;
	message_id: string;
}

interface AgentActionWirePayload {
	session_id: string;
	tool_name: string;
	input: unknown;
	step_number: number;
	run_id: number;
	tool_call_id: string | null;
	action_index: number;
	step_id: string;
	suppress_streamed_thought: boolean;
	silent: boolean;
}

interface AgentObservationWirePayload {
	session_id: string;
	observation: string;
	tool_name: string;
	step_number: number;
	run_id: number;
	silent: boolean;
	tool_call_id: string | null;
	action_index: number;
	ask_options: string[];
	step_id: string;
}

interface AgentChunkWirePayload {
	session_id: string;
	delta: string;
	step_number: number;
	run_id: number;
	message_id: string;
	seq: number;
}
interface AgentStreamResetWirePayload {
	session_id: string;
	step_number: number;
	run_id: number;
	thought_message_id: string;
	reasoning_message_id: string;
}
interface AgentWebSearchWirePayload {
	session_id: string;
	phase: string;
	step_number: number;
	run_id: number;
	call_id?: string;
	action?: string;
	result?: unknown;
}
interface AgentStreamStalledWirePayload { session_id: string; }
interface AgentSupplementWirePayload {
	session_id: string;
	additional_context: string;
	step_number: number;
	run_id: number;
	inject_source?: string;
}
interface AgentCompactionWirePayload {
	session_id: string;
	summary: string;
	tokens_before: number;
	tokens_after: number;
	degraded?: boolean;
	episode_id?: string;
}
interface AgentNotificationWirePayload { session_id: string; title: string; body: string; }
interface AgentUsageWirePayload {
	session_id: string;
	prompt_tokens: number;
	completion_tokens: number;
	total_tokens: number;
	cached_tokens: number;
	cache_creation_tokens: number;
	cache_miss_tokens: number;
	context_tokens: number;
	cache_exclusive: boolean;
	cache_accounting: string;
	cost_usd: number | null;
	model: string | null;
	cumulative_prompt_tokens: number;
	cumulative_completion_tokens: number;
	cumulative_total_tokens: number;
	cumulative_cached_tokens: number;
	cumulative_cache_creation_tokens: number;
	cumulative_cache_miss_tokens: number;
	cache_diagnostics?: unknown;
	cumulative_cost_usd: number | null;
	context_window: number | null;
	step_number?: number;
	duration_ms?: number;
	role?: string;
	has_cost: boolean;
}
interface AgentToolOutputWirePayload {
	session_id: string;
	step_id: string;
	output: string;
}

interface AgentWirePayloadMap {
	'agent:thought': AgentThoughtWirePayload;
	'agent:action': AgentActionWirePayload;
	'agent:observation': AgentObservationWirePayload;
	'agent:thought_chunk': AgentChunkWirePayload;
	'agent:reasoning_chunk': AgentChunkWirePayload;
	'agent:stream_reset': AgentStreamResetWirePayload;
	'agent:web_search': AgentWebSearchWirePayload;
	'agent:stream_stalled': AgentStreamStalledWirePayload;
	'agent:supplement': AgentSupplementWirePayload;
	'agent:compaction': AgentCompactionWirePayload;
	'agent:usage': AgentUsageWirePayload;
	'agent:tool_output': AgentToolOutputWirePayload;
	'notification:show': AgentNotificationWirePayload;
}

/** Convert one known agent event from the Rust/Tauri wire shape. */
export function mapAgentEvent<K extends AgentEventName>(
	event: TauriEvent<AgentWirePayloadMap[K]> & { event: K },
): TauriEvent<AgentEventPayloadMap[K]> {
	const p = event.payload;
	switch (event.event) {
		case 'agent:thought': {
			const payload = p as AgentThoughtWirePayload;
			return { ...event, payload: {
				sessionId: payload.session_id,
				thought: payload.thought,
				stepNumber: payload.step_number,
				runId: payload.run_id,
				messageId: payload.message_id,
			} } as unknown as TauriEvent<AgentEventPayloadMap[K]>;
		}
		case 'agent:action': {
			const payload = p as AgentActionWirePayload;
			return { ...event, payload: {
				sessionId: payload.session_id,
				toolName: payload.tool_name,
				input: payload.input,
				stepNumber: payload.step_number,
				runId: payload.run_id,
				toolCallId: payload.tool_call_id,
				actionIndex: payload.action_index,
				stepId: payload.step_id,
				suppressStreamedThought: payload.suppress_streamed_thought,
				silent: payload.silent,
			} } as unknown as TauriEvent<AgentEventPayloadMap[K]>;
		}
		case 'agent:observation': {
			const payload = p as AgentObservationWirePayload;
			return { ...event, payload: {
				sessionId: payload.session_id,
				observation: payload.observation,
				toolName: payload.tool_name,
				stepNumber: payload.step_number,
				runId: payload.run_id,
				silent: payload.silent,
				toolCallId: payload.tool_call_id,
				actionIndex: payload.action_index,
				askOptions: payload.ask_options,
				stepId: payload.step_id,
			} } as unknown as TauriEvent<AgentEventPayloadMap[K]>;
		}
		case 'agent:thought_chunk':
		case 'agent:reasoning_chunk': {
			const payload = p as AgentChunkWirePayload;
			return { ...event, payload: {
				sessionId: payload.session_id,
				delta: payload.delta,
				stepNumber: payload.step_number,
				runId: payload.run_id,
				messageId: payload.message_id,
				seq: payload.seq,
			} } as unknown as TauriEvent<AgentEventPayloadMap[K]>;
		}
		case 'agent:stream_reset': {
			const payload = p as AgentStreamResetWirePayload;
			return { ...event, payload: {
				sessionId: payload.session_id,
				stepNumber: payload.step_number,
				runId: payload.run_id,
				thoughtMessageId: payload.thought_message_id,
				reasoningMessageId: payload.reasoning_message_id,
			} } as unknown as TauriEvent<AgentEventPayloadMap[K]>;
		}
		case 'agent:web_search': {
			const payload = p as AgentWebSearchWirePayload;
			return { ...event, payload: {
				sessionId: payload.session_id,
				phase: payload.phase,
				stepNumber: payload.step_number,
				runId: payload.run_id,
				...(payload.call_id !== undefined ? { callId: payload.call_id } : {}),
				...(payload.action !== undefined ? { action: payload.action } : {}),
				...(payload.result !== undefined ? { result: payload.result } : {}),
			} } as unknown as TauriEvent<AgentEventPayloadMap[K]>;
		}
		case 'agent:stream_stalled': {
			const payload = p as AgentStreamStalledWirePayload;
			return { ...event, payload: { sessionId: payload.session_id } } as unknown as
				TauriEvent<AgentEventPayloadMap[K]>;
		}
		case 'agent:supplement': {
			const payload = p as AgentSupplementWirePayload;
			return { ...event, payload: {
				sessionId: payload.session_id,
				additionalContext: payload.additional_context,
				stepNumber: payload.step_number,
				runId: payload.run_id,
				...(payload.inject_source !== undefined ? { injectSource: payload.inject_source } : {}),
			} } as unknown as TauriEvent<AgentEventPayloadMap[K]>;
		}
		case 'agent:compaction': {
			const payload = p as AgentCompactionWirePayload;
			return { ...event, payload: {
				sessionId: payload.session_id,
				summary: payload.summary,
				tokensBefore: payload.tokens_before,
				tokensAfter: payload.tokens_after,
				degraded: payload.degraded ?? false,
				...(payload.episode_id !== undefined ? { episodeId: payload.episode_id } : {}),
			} } as unknown as TauriEvent<AgentEventPayloadMap[K]>;
		}
		case 'agent:usage': {
			const payload = p as AgentUsageWirePayload;
			return { ...event, payload: {
				sessionId: payload.session_id,
				promptTokens: payload.prompt_tokens,
				completionTokens: payload.completion_tokens,
				totalTokens: payload.total_tokens,
				cachedTokens: payload.cached_tokens,
				cacheCreationTokens: payload.cache_creation_tokens,
				cacheMissTokens: payload.cache_miss_tokens,
				contextTokens: payload.context_tokens,
				cacheExclusive: payload.cache_exclusive,
				cacheAccounting: payload.cache_accounting,
				costUsd: payload.cost_usd,
				model: payload.model,
				cumulativePromptTokens: payload.cumulative_prompt_tokens,
				cumulativeCompletionTokens: payload.cumulative_completion_tokens,
				cumulativeTotalTokens: payload.cumulative_total_tokens,
				cumulativeCachedTokens: payload.cumulative_cached_tokens,
				cumulativeCacheCreationTokens: payload.cumulative_cache_creation_tokens,
				cumulativeCacheMissTokens: payload.cumulative_cache_miss_tokens,
				...(payload.cache_diagnostics !== undefined ? { cacheDiagnostics: payload.cache_diagnostics } : {}),
				cumulativeCostUsd: payload.cumulative_cost_usd,
				contextWindow: payload.context_window,
				...(payload.step_number !== undefined ? { stepNumber: payload.step_number } : {}),
				...(payload.duration_ms !== undefined ? { durationMs: payload.duration_ms } : {}),
				...(payload.role !== undefined ? { role: payload.role } : {}),
				hasCost: payload.has_cost,
			} } as unknown as TauriEvent<AgentEventPayloadMap[K]>;
		}
		case 'agent:tool_output': {
			const payload = p as AgentToolOutputWirePayload;
			return { ...event, payload: {
				sessionId: payload.session_id,
				stepId: payload.step_id,
				output: payload.output,
			} } as unknown as TauriEvent<AgentEventPayloadMap[K]>;
		}
		case 'notification:show': {
			const payload = p as AgentNotificationWirePayload;
			return { ...event, payload: {
				sessionId: payload.session_id,
				title: payload.title,
				body: payload.body,
			} } as unknown as TauriEvent<AgentEventPayloadMap[K]>;
		}
	}
}
