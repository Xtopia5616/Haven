/** Agent event IPC contract at the frontend boundary. */

import type {
	AgentNotificationKind,
	CacheAccounting,
	LlmCallKind,
	MediaInputStrategy,
	MediaPlanNoticeCode,
	MediaProjectionMode,
	MediaRepresentationKind,
	OperationIdempotency,
	RequestKind,
	ToolErrorClass,
	ToolExecutionOutcome,
	ToolOperationScope,
	ToolRetryability,
	ToolRunCompletionStatusDto,
	ToolRunKindDto,
} from './generatedCommands.ts';
import {
	AGENT_EVENT_NAMES,
	AGENT_NOTIFICATION_KIND_VALUES,
	CACHE_ACCOUNTING_VALUES,
	LLM_CALL_KIND_VALUES,
	MEDIA_INPUT_STRATEGY_VALUES,
	MEDIA_PLAN_NOTICE_CODE_VALUES,
	MEDIA_PROJECTION_MODE_VALUES,
	MEDIA_REPRESENTATION_KIND_VALUES,
	OPERATION_IDEMPOTENCY_VALUES,
	REQUEST_KIND_VALUES,
	TOOL_ERROR_CLASS_VALUES,
	TOOL_EXECUTION_OUTCOME_VALUES,
	TOOL_OPERATION_SCOPE_VALUES,
	TOOL_RETRYABILITY_VALUES,
	TOOL_RUN_COMPLETION_STATUS_DTO_VALUES,
	TOOL_RUN_KIND_DTO_VALUES,
} from './generatedCommands.ts';
import type { TauriEvent } from './tauriEvent.ts';
import { isRecord } from './objectGuards.ts';

export type AgentEventName = (typeof AGENT_EVENT_NAMES)[number];

export interface AgentThoughtPayload {
	sessionId: string;
	thought: string;
	stepNumber: number;
	runId: number;
	messageId: string;
	eventSeq?: number;
}

export interface AgentToolCallPayload {
	sessionId: string;
	toolName: string;
	input: unknown;
	stepNumber: number;
	runId: number;
	toolCallId: string | null;
	toolIndex: number;
	stepId: string;
	suppressStreamedThought: boolean;
	silent: boolean;
	eventSeq?: number;
}

export interface AgentObservationPayload {
	sessionId: string;
	observation: string;
	toolName: string;
	stepNumber: number;
	runId: number;
	silent: boolean;
	toolCallId: string | null;
	toolIndex: number;
	askOptions: string[];
	stepId: string;
	idempotency: OperationIdempotency;
	operationScope: ToolOperationScope;
	renderer: string;
	result: AgentToolResultEnvelope;
	eventSeq?: number;
}

export interface AgentToolResultEnvelope {
	outcome: ToolExecutionOutcome;
	errorClass?: ToolErrorClass | null;
	retrySafety: OperationIdempotency;
	retryability: ToolRetryability;
	verificationHint?: string | null;
	nextAction?: string | null;
	assets: string[];
}

export interface AgentChunkPayload {
	sessionId: string;
	delta: string;
	stepNumber: number;
	runId: number;
	messageId: string;
	seq: number;
}

export interface AgentToolCallChunkPayload {
	sessionId: string;
	previewId: string;
	toolName: string;
	arguments: string;
	argumentsTruncated: boolean;
	stepNumber: number;
	runId: number;
	toolIndex: number;
	seq: number;
}

export interface AgentStreamResetPayload {
	sessionId: string;
	stepNumber: number;
	runId: number;
	thoughtMessageId: string;
	reasoningMessageId: string;
}

export interface AgentMediaPlanPayload {
	sessionId: string;
	stepNumber: number;
	runId: number;
	/** RequestKind string under the established `role` wire field. */
	role: RequestKind;
	strategy: MediaInputStrategy;
	projections: Array<{
		assetId: string;
		representation: MediaRepresentationKind;
		mode: MediaProjectionMode;
	}>;
	notices: Array<{ assetId: string; code: MediaPlanNoticeCode }>;
	eventSeq?: number;
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
	messageId?: string;
	supplementId: string;
	injectSource?: string;
	eventSeq?: number;
}

export interface AgentCompactionPayload {
	sessionId: string;
	summary: string;
	tokensBefore: number;
	tokensAfter: number;
	degraded: boolean;
	episodeId?: string;
	eventSeq?: number;
}

export interface AgentNotificationPayload {
	sessionId?: string;
	title: string;
	body: string;
	notificationKind?: AgentNotificationKind;
	toolRunKind?: ToolRunKindDto;
	toolRunId?: string;
	toolRunStatus?: ToolRunCompletionStatusDto;
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
	cacheAccounting: CacheAccounting;
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
	role?: RequestKind;
	callKind: LlmCallKind;
	hasCost: boolean;
}

export interface AgentToolOutputPayload {
	sessionId: string;
	stepId: string;
	output: string;
}

export interface AgentEventPayloadMap {
	'agent:thought': AgentThoughtPayload;
	'agent:tool_call': AgentToolCallPayload;
	'agent:observation': AgentObservationPayload;
	'agent:thought_chunk': AgentChunkPayload;
	'agent:reasoning_chunk': AgentChunkPayload;
	'agent:tool_call_chunk': AgentToolCallChunkPayload;
	'agent:stream_reset': AgentStreamResetPayload;
	'agent:media_plan': AgentMediaPlanPayload;
	'agent:web_search': AgentWebSearchPayload;
	'agent:stream_stalled': AgentStreamStalledPayload;
	'agent:supplement': AgentSupplementPayload;
	'agent:compaction': AgentCompactionPayload;
	'agent:usage': AgentUsagePayload;
	'agent:tool_output': AgentToolOutputPayload;
	'notification:show': AgentNotificationPayload;
}

type WireRecord = Record<string, unknown>;

const AGENT_EVENT_NAME_SET = new Set<string>(AGENT_EVENT_NAMES);

function hasOwn(record: WireRecord, field: string): boolean {
	return Object.prototype.hasOwnProperty.call(record, field);
}

function requiredString(record: WireRecord, field: string): string | null {
	return typeof record[field] === 'string' ? (record[field] as string) : null;
}

function requiredSessionId(record: WireRecord): string | null {
	const value = requiredString(record, 'session_id');
	return value && value.length > 0 ? value : null;
}

function optionalSessionId(record: WireRecord): string | undefined | null {
	if (!hasOwn(record, 'session_id')) return undefined;
	const value = requiredString(record, 'session_id');
	return value && value.length > 0 ? value : null;
}

function finiteNumber(value: unknown): value is number {
	return typeof value === 'number' && Number.isFinite(value);
}

function isGeneratedValue<T extends string>(values: readonly T[], value: unknown): value is T {
	return (values as readonly unknown[]).includes(value);
}

function requiredNumber(record: WireRecord, field: string): number | null {
	return finiteNumber(record[field]) ? record[field] : null;
}

function requiredBoolean(record: WireRecord, field: string): boolean | null {
	return typeof record[field] === 'boolean' ? record[field] : null;
}

function optionalStringIsValid(record: WireRecord, field: string): boolean {
	return record[field] === undefined || typeof record[field] === 'string';
}

function optionalNumberIsValid(record: WireRecord, field: string): boolean {
	return record[field] === undefined || finiteNumber(record[field]);
}

function nullableStringIsValid(value: unknown): boolean {
	return value === null || typeof value === 'string';
}

function nullableNumberIsValid(value: unknown): boolean {
	return value === null || finiteNumber(value);
}

function stringArray(value: unknown): value is string[] {
	return Array.isArray(value) && value.every((item) => typeof item === 'string');
}

function isOneOf<const Values extends readonly string[]>(
	value: unknown,
	values: Values,
): value is Values[number] {
	return typeof value === 'string' && values.includes(value);
}

function mapToolResult(value: unknown): AgentToolResultEnvelope | null {
	if (!isRecord(value)) return null;
	const outcome = requiredString(value, 'outcome');
	const retrySafety = requiredString(value, 'retry_safety');
	const retryability = requiredString(value, 'retryability');
	const errorClass = value.error_class;
	if (
		!isOneOf(outcome, TOOL_EXECUTION_OUTCOME_VALUES) ||
		!isOneOf(retrySafety, OPERATION_IDEMPOTENCY_VALUES) ||
		!isOneOf(retryability, TOOL_RETRYABILITY_VALUES) ||
		(errorClass !== undefined &&
			errorClass !== null &&
			!isOneOf(errorClass, TOOL_ERROR_CLASS_VALUES)) ||
		!stringArray(value.assets) ||
		!['verification_hint', 'next_action'].every(
			(field) => value[field] === undefined || nullableStringIsValid(value[field]),
		)
	) {
		return null;
	}
	return {
		outcome,
		errorClass: errorClass as ToolErrorClass | null | undefined,
		retrySafety,
		retryability,
		verificationHint: value.verification_hint as string | null | undefined,
		nextAction: value.next_action as string | null | undefined,
		assets: value.assets,
	};
}

/**
 * Validate one Tauri agent event and map its Rust snake_case payload to the
 * route-facing camelCase DTO. Unknown additive payload fields are ignored;
 * enum-like fields must match the current Rust wire variants.
 */
export function mapAgentEvent<K extends AgentEventName>(
	event: TauriEvent<unknown> & { event: K },
): TauriEvent<AgentEventPayloadMap[K]> | null;
export function mapAgentEvent(
	event: unknown,
): TauriEvent<AgentEventPayloadMap[AgentEventName]> | null;
export function mapAgentEvent(
	event: unknown,
): TauriEvent<AgentEventPayloadMap[AgentEventName]> | null {
	if (
		!isRecord(event) ||
		typeof event.event !== 'string' ||
		!AGENT_EVENT_NAME_SET.has(event.event) ||
		!finiteNumber(event.id) ||
		!isRecord(event.payload)
	) {
		return null;
	}

	const tauriEvent = event as unknown as TauriEvent<unknown>;
	const eventName = event.event;
	const payload = event.payload;
	switch (eventName) {
		case 'agent:thought': {
			const sessionId = requiredSessionId(payload);
			const thought = requiredString(payload, 'thought');
			const stepNumber = requiredNumber(payload, 'step_number');
			const runId = requiredNumber(payload, 'run_id');
			const messageId = requiredString(payload, 'message_id');
			if (
				sessionId === null ||
				thought === null ||
				stepNumber === null ||
				runId === null ||
				messageId === null ||
				!optionalNumberIsValid(payload, 'event_seq')
			)
				return null;
			return {
				...tauriEvent,
				payload: {
					sessionId,
					thought,
					stepNumber,
					runId,
					messageId,
					...(payload.event_seq !== undefined
						? { eventSeq: payload.event_seq as number }
						: {}),
				},
			};
		}
		case 'agent:tool_call': {
			const sessionId = requiredSessionId(payload);
			const toolName = requiredString(payload, 'tool_name');
			const stepNumber = requiredNumber(payload, 'step_number');
			const runId = requiredNumber(payload, 'run_id');
			const toolIndex = requiredNumber(payload, 'tool_index');
			const stepId = requiredString(payload, 'step_id');
			const suppressStreamedThought = requiredBoolean(payload, 'suppress_streamed_thought');
			const silent = requiredBoolean(payload, 'silent');
			const toolCallId = payload.tool_call_id;
			if (
				sessionId === null ||
				toolName === null ||
				stepNumber === null ||
				runId === null ||
				toolIndex === null ||
				stepId === null ||
				suppressStreamedThought === null ||
				silent === null ||
				!hasOwn(payload, 'input') ||
				(toolCallId !== null && typeof toolCallId !== 'string') ||
				!optionalNumberIsValid(payload, 'event_seq')
			)
				return null;
			return {
				...tauriEvent,
				payload: {
					sessionId,
					toolName,
					input: payload.input,
					stepNumber,
					runId,
					toolCallId,
					toolIndex,
					stepId,
					suppressStreamedThought,
					silent,
					...(payload.event_seq !== undefined
						? { eventSeq: payload.event_seq as number }
						: {}),
				},
			};
		}
		case 'agent:observation': {
			const sessionId = requiredSessionId(payload);
			const observation = requiredString(payload, 'observation');
			const toolName = requiredString(payload, 'tool_name');
			const stepNumber = requiredNumber(payload, 'step_number');
			const runId = requiredNumber(payload, 'run_id');
			const silent = requiredBoolean(payload, 'silent');
			const toolIndex = requiredNumber(payload, 'tool_index');
			const askOptions = stringArray(payload.ask_options) ? payload.ask_options : null;
			const stepId = requiredString(payload, 'step_id');
			const idempotency = payload.idempotency;
			const operationScope = payload.operation_scope;
			const renderer = requiredString(payload, 'renderer');
			const toolCallId = payload.tool_call_id;
			const result = mapToolResult(payload.result);
			if (
				sessionId === null ||
				observation === null ||
				toolName === null ||
				stepNumber === null ||
				runId === null ||
				silent === null ||
				toolIndex === null ||
				askOptions === null ||
				stepId === null ||
				renderer === null ||
				!isOneOf(idempotency, OPERATION_IDEMPOTENCY_VALUES) ||
				!isOneOf(operationScope, TOOL_OPERATION_SCOPE_VALUES) ||
				(toolCallId !== null && typeof toolCallId !== 'string') ||
				result === null ||
				!optionalNumberIsValid(payload, 'event_seq')
			)
				return null;
			return {
				...tauriEvent,
				payload: {
					sessionId,
					observation,
					toolName,
					stepNumber,
					runId,
					silent,
					toolCallId,
					toolIndex,
					askOptions,
					stepId,
					idempotency,
					operationScope,
					renderer,
					result,
					...(payload.event_seq !== undefined
						? { eventSeq: payload.event_seq as number }
						: {}),
				},
			};
		}
		case 'agent:thought_chunk':
		case 'agent:reasoning_chunk': {
			const sessionId = requiredSessionId(payload);
			const delta = requiredString(payload, 'delta');
			const stepNumber = requiredNumber(payload, 'step_number');
			const runId = requiredNumber(payload, 'run_id');
			const messageId = requiredString(payload, 'message_id');
			const seq = requiredNumber(payload, 'seq');
			if (
				sessionId === null ||
				delta === null ||
				stepNumber === null ||
				runId === null ||
				messageId === null ||
				seq === null
			)
				return null;
			return {
				...tauriEvent,
				payload: { sessionId, delta, stepNumber, runId, messageId, seq },
			};
		}
		case 'agent:tool_call_chunk': {
			const sessionId = requiredSessionId(payload);
			const previewId = requiredString(payload, 'preview_id');
			const toolName = requiredString(payload, 'tool_name');
			const argumentsText = requiredString(payload, 'arguments');
			const argumentsTruncated = requiredBoolean(payload, 'arguments_truncated');
			const stepNumber = requiredNumber(payload, 'step_number');
			const runId = requiredNumber(payload, 'run_id');
			const toolIndex = requiredNumber(payload, 'tool_index');
			const seq = requiredNumber(payload, 'seq');
			if (
				sessionId === null ||
				previewId === null ||
				toolName === null ||
				argumentsText === null ||
				argumentsTruncated === null ||
				stepNumber === null ||
				runId === null ||
				toolIndex === null ||
				seq === null
			)
				return null;
			return {
				...tauriEvent,
				payload: {
					sessionId,
					previewId,
					toolName,
					arguments: argumentsText,
					argumentsTruncated,
					stepNumber,
					runId,
					toolIndex,
					seq,
				},
			};
		}
		case 'agent:stream_reset': {
			const sessionId = requiredSessionId(payload);
			const stepNumber = requiredNumber(payload, 'step_number');
			const runId = requiredNumber(payload, 'run_id');
			const thoughtMessageId = requiredString(payload, 'thought_message_id');
			const reasoningMessageId = requiredString(payload, 'reasoning_message_id');
			if (
				sessionId === null ||
				stepNumber === null ||
				runId === null ||
				thoughtMessageId === null ||
				reasoningMessageId === null
			)
				return null;
			return {
				...tauriEvent,
				payload: { sessionId, stepNumber, runId, thoughtMessageId, reasoningMessageId },
			};
		}
		case 'agent:web_search': {
			const sessionId = requiredSessionId(payload);
			const phase = requiredString(payload, 'phase');
			const stepNumber = requiredNumber(payload, 'step_number');
			const runId = requiredNumber(payload, 'run_id');
			if (
				sessionId === null ||
				phase === null ||
				stepNumber === null ||
				runId === null ||
				!optionalStringIsValid(payload, 'call_id') ||
				!optionalStringIsValid(payload, 'action')
			)
				return null;
			return {
				...tauriEvent,
				payload: {
					sessionId,
					phase,
					stepNumber,
					runId,
					...(payload.call_id !== undefined ? { callId: payload.call_id as string } : {}),
					...(payload.action !== undefined ? { action: payload.action as string } : {}),
					...(payload.result !== undefined ? { result: payload.result } : {}),
				},
			};
		}
		case 'agent:stream_stalled': {
			const sessionId = requiredSessionId(payload);
			return sessionId === null ? null : { ...tauriEvent, payload: { sessionId } };
		}
		case 'agent:media_plan': {
			const sessionId = requiredSessionId(payload);
			const stepNumber = requiredNumber(payload, 'step_number');
			const runId = requiredNumber(payload, 'run_id');
			const role = payload.role;
			const strategy = payload.strategy;
			const projections =
				Array.isArray(payload.projections) &&
				payload.projections.every(
					(item) =>
						isRecord(item) &&
						typeof item.asset_id === 'string' &&
						isOneOf(item.representation, MEDIA_REPRESENTATION_KIND_VALUES) &&
						isOneOf(item.mode, MEDIA_PROJECTION_MODE_VALUES),
				)
					? payload.projections.map((item) => ({
							assetId: (item as WireRecord).asset_id as string,
							representation: (item as WireRecord)
								.representation as MediaRepresentationKind,
							mode: (item as WireRecord).mode as MediaProjectionMode,
						}))
					: null;
			const notices =
				Array.isArray(payload.notices) &&
				payload.notices.every(
					(item) =>
						isRecord(item) &&
						typeof item.asset_id === 'string' &&
						isOneOf(item.code, MEDIA_PLAN_NOTICE_CODE_VALUES),
				)
					? payload.notices.map((item) => ({
							assetId: (item as WireRecord).asset_id as string,
							code: (item as WireRecord).code as MediaPlanNoticeCode,
						}))
					: null;
			if (
				sessionId === null ||
				stepNumber === null ||
				runId === null ||
				!isOneOf(role, REQUEST_KIND_VALUES) ||
				!isOneOf(strategy, MEDIA_INPUT_STRATEGY_VALUES) ||
				projections === null ||
				notices === null ||
				!optionalNumberIsValid(payload, 'event_seq')
			)
				return null;
			return {
				...tauriEvent,
				payload: {
					sessionId,
					stepNumber,
					runId,
					role,
					strategy,
					projections,
					notices,
					...(payload.event_seq !== undefined
						? { eventSeq: payload.event_seq as number }
						: {}),
				},
			};
		}
		case 'agent:supplement': {
			const sessionId = requiredSessionId(payload);
			const additionalContext = requiredString(payload, 'additional_context');
			const stepNumber = requiredNumber(payload, 'step_number');
			const runId = requiredNumber(payload, 'run_id');
			const supplementId = requiredString(payload, 'supplement_id');
			if (
				sessionId === null ||
				additionalContext === null ||
				stepNumber === null ||
				runId === null ||
				supplementId === null ||
				!optionalStringIsValid(payload, 'message_id') ||
				!optionalStringIsValid(payload, 'inject_source') ||
				!optionalNumberIsValid(payload, 'event_seq')
			)
				return null;
			return {
				...tauriEvent,
				payload: {
					sessionId,
					additionalContext,
					stepNumber,
					runId,
					...(payload.message_id !== undefined
						? { messageId: payload.message_id as string }
						: {}),
					supplementId,
					...(payload.inject_source !== undefined
						? { injectSource: payload.inject_source as string }
						: {}),
					...(payload.event_seq !== undefined
						? { eventSeq: payload.event_seq as number }
						: {}),
				},
			};
		}
		case 'agent:compaction': {
			const sessionId = requiredSessionId(payload);
			const summary = requiredString(payload, 'summary');
			const tokensBefore = requiredNumber(payload, 'tokens_before');
			const tokensAfter = requiredNumber(payload, 'tokens_after');
			const degraded = requiredBoolean(payload, 'degraded');
			if (
				sessionId === null ||
				summary === null ||
				tokensBefore === null ||
				tokensAfter === null ||
				degraded === null ||
				!optionalStringIsValid(payload, 'episode_id') ||
				!optionalNumberIsValid(payload, 'event_seq')
			)
				return null;
			return {
				...tauriEvent,
				payload: {
					sessionId,
					summary,
					tokensBefore,
					tokensAfter,
					degraded,
					...(payload.episode_id !== undefined
						? { episodeId: payload.episode_id as string }
						: {}),
					...(payload.event_seq !== undefined
						? { eventSeq: payload.event_seq as number }
						: {}),
				},
			};
		}
		case 'agent:usage': {
			const sessionId = requiredSessionId(payload);
			const cacheAccounting = payload.cache_accounting;
			const role = payload.role;
			const numberFields = [
				'prompt_tokens',
				'completion_tokens',
				'total_tokens',
				'cached_tokens',
				'cache_creation_tokens',
				'cache_miss_tokens',
				'context_tokens',
				'cumulative_prompt_tokens',
				'cumulative_completion_tokens',
				'cumulative_total_tokens',
				'cumulative_cached_tokens',
				'cumulative_cache_creation_tokens',
				'cumulative_cache_miss_tokens',
			] as const;
			if (
				sessionId === null ||
				!numberFields.every((field) => finiteNumber(payload[field])) ||
				typeof payload.cache_exclusive !== 'boolean' ||
				!isOneOf(cacheAccounting, CACHE_ACCOUNTING_VALUES) ||
				!nullableNumberIsValid(payload.cost_usd) ||
				!nullableStringIsValid(payload.model) ||
				!nullableNumberIsValid(payload.cumulative_cost_usd) ||
				!nullableNumberIsValid(payload.context_window) ||
				!optionalNumberIsValid(payload, 'step_number') ||
				!optionalNumberIsValid(payload, 'duration_ms') ||
				(role !== undefined && !isOneOf(role, REQUEST_KIND_VALUES)) ||
				!isOneOf(payload.call_kind, LLM_CALL_KIND_VALUES) ||
				typeof payload.has_cost !== 'boolean'
			)
				return null;
			return {
				...tauriEvent,
				payload: {
					sessionId,
					promptTokens: payload.prompt_tokens as number,
					completionTokens: payload.completion_tokens as number,
					totalTokens: payload.total_tokens as number,
					cachedTokens: payload.cached_tokens as number,
					cacheCreationTokens: payload.cache_creation_tokens as number,
					cacheMissTokens: payload.cache_miss_tokens as number,
					contextTokens: payload.context_tokens as number,
					cacheExclusive: payload.cache_exclusive,
					cacheAccounting,
					costUsd: payload.cost_usd as number | null,
					model: payload.model as string | null,
					cumulativePromptTokens: payload.cumulative_prompt_tokens as number,
					cumulativeCompletionTokens: payload.cumulative_completion_tokens as number,
					cumulativeTotalTokens: payload.cumulative_total_tokens as number,
					cumulativeCachedTokens: payload.cumulative_cached_tokens as number,
					cumulativeCacheCreationTokens:
						payload.cumulative_cache_creation_tokens as number,
					cumulativeCacheMissTokens: payload.cumulative_cache_miss_tokens as number,
					...(payload.cache_diagnostics !== undefined
						? { cacheDiagnostics: payload.cache_diagnostics }
						: {}),
					cumulativeCostUsd: payload.cumulative_cost_usd as number | null,
					contextWindow: payload.context_window as number | null,
					...(payload.step_number !== undefined
						? { stepNumber: payload.step_number as number }
						: {}),
					...(payload.duration_ms !== undefined
						? { durationMs: payload.duration_ms as number }
						: {}),
					...(role !== undefined ? { role } : {}),
					callKind: payload.call_kind,
					hasCost: payload.has_cost,
				},
			};
		}
		case 'agent:tool_output': {
			const sessionId = requiredSessionId(payload);
			const stepId = requiredString(payload, 'step_id');
			const output = requiredString(payload, 'output');
			return sessionId === null || stepId === null || output === null
				? null
				: { ...tauriEvent, payload: { sessionId, stepId, output } };
		}
		case 'notification:show': {
			const notificationKind = payload.notification_kind;
			if (
				notificationKind !== undefined &&
				!isGeneratedValue(AGENT_NOTIFICATION_KIND_VALUES, notificationKind)
			)
				return null;
			if (notificationKind === 'tool_run_completion') {
				const sessionId = optionalSessionId(payload);
				const title = requiredString(payload, 'title');
				const body = requiredString(payload, 'body');
				const toolRunKind = payload.tool_run_kind;
				const toolRunId = requiredString(payload, 'tool_run_id');
				const toolRunStatus = payload.tool_run_status;
				if (
					toolRunStatus !== undefined &&
					!isGeneratedValue(TOOL_RUN_COMPLETION_STATUS_DTO_VALUES, toolRunStatus)
				)
					return null;
				if (
					sessionId === null ||
					title === null ||
					body === null ||
					!isGeneratedValue(TOOL_RUN_KIND_DTO_VALUES, toolRunKind) ||
					toolRunId === null ||
					toolRunId.length === 0 ||
					(toolRunKind === 'background' &&
						(sessionId === undefined || toolRunStatus === undefined)) ||
					(toolRunKind === 'scheduled' && toolRunStatus !== undefined)
				)
					return null;
				return {
					...tauriEvent,
					payload: {
						...(sessionId !== undefined ? { sessionId } : {}),
						title,
						body,
						notificationKind,
						toolRunKind,
						toolRunId,
						...(toolRunStatus !== undefined ? { toolRunStatus } : {}),
					},
				};
			}
			if (notificationKind !== undefined) return null;

			const sessionId = optionalSessionId(payload);
			const title = requiredString(payload, 'title');
			const body = requiredString(payload, 'body');
			return sessionId === null || title === null || body === null
				? null
				: {
						...tauriEvent,
						payload: {
							...(sessionId !== undefined ? { sessionId } : {}),
							title,
							body,
						},
					};
		}
		default:
			return null;
	}
}
