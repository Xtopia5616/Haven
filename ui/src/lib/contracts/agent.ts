/** Agent event IPC contract at the frontend boundary. */

import type {
	AgentNotificationKind,
	CacheAccounting,
	CacheDiagnostics,
	InjectSource,
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
	WebSearchPhase,
} from './generatedCommands.ts';
import {
	AGENT_EVENT_NAMES,
	AGENT_NOTIFICATION_KIND_VALUES,
	CACHE_ACCOUNTING_VALUES,
	CACHE_DIAGNOSTIC_OUTCOME_VALUES,
	CACHE_USAGE_SOURCE_VALUES,
	INJECT_SOURCE_VALUES,
	LLM_CALL_KIND_VALUES,
	MEDIA_INPUT_STRATEGY_VALUES,
	MEDIA_PLAN_NOTICE_CODE_VALUES,
	MEDIA_PROJECTION_MODE_VALUES,
	MEDIA_REPRESENTATION_KIND_VALUES,
	OPERATION_IDEMPOTENCY_VALUES,
	PROMPT_CACHE_STRATEGY_VALUES,
	REQUEST_KIND_VALUES,
	TOOL_ERROR_CLASS_VALUES,
	TOOL_EXECUTION_OUTCOME_VALUES,
	TOOL_OPERATION_SCOPE_VALUES,
	TOOL_RETRYABILITY_VALUES,
	TOOL_RUN_COMPLETION_STATUS_DTO_VALUES,
	TOOL_RUN_KIND_DTO_VALUES,
	WEB_SEARCH_PHASE_VALUES,
} from './generatedCommands.ts';
import { isValidTauriEventId, type TauriEvent } from './tauriEvent.ts';
import { isRecord } from './objectGuards.ts';
import { matchesGeneratedEventPayload } from './generatedWireShape.ts';
import {
	isBoolean,
	isFiniteNumber,
	isOneOf,
	isSafeUnsignedInteger,
	isString,
	isStringArray,
	isU32,
} from './valueGuards.ts';
import {
	canonicalIdField,
	hasOwnWireField,
	isCanonicalEntityId,
	optionalCanonicalIdFieldIsValid,
	optionalStringFieldIsValid,
	readStringField,
	type WireRecord,
} from './wireGuards.ts';

export type AgentEventName = (typeof AGENT_EVENT_NAMES)[number];

export interface AgentThoughtPayload {
	sessionId: string;
	thought: string;
	stepNumber: number;
	runId: number;
	messageId: string;
	eventSequence?: number;
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
	eventSequence?: number;
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
	eventSequence?: number;
}

export interface AgentToolResultEnvelope {
	outcome: ToolExecutionOutcome;
	errorClass?: ToolErrorClass | null;
	idempotency: OperationIdempotency;
	retryability: ToolRetryability;
	verificationHint?: string | null;
	nextAction?: string | null;
	assets: string[];
	sandboxOutputReviewStepIds?: string[];
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
	requestKind: RequestKind;
	strategy: MediaInputStrategy;
	projections: Array<{
		assetId: string;
		representation: MediaRepresentationKind;
		mode: MediaProjectionMode;
	}>;
	notices: Array<{ assetId: string; code: MediaPlanNoticeCode }>;
	eventSequence?: number;
}

export interface AgentWebSearchPayload {
	sessionId: string;
	phase: WebSearchPhase;
	stepNumber: number;
	runId: number;
	callId?: string;
	callIndex?: number;
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
	injectSource?: InjectSource;
	eventSequence?: number;
}

export interface AgentCompactionPayload {
	sessionId: string;
	summary: string;
	tokensBefore: number;
	tokensAfter: number;
	degraded: boolean;
	episodeId?: string;
	eventSequence?: number;
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
	cacheDiagnostics?: CacheDiagnostics;
	cumulativeCostUsd: number | null;
	contextWindow: number | null;
	stepNumber?: number;
	durationMs?: number;
	requestKind?: RequestKind;
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

const AGENT_EVENT_NAME_SET = new Set<string>(AGENT_EVENT_NAMES);

function optionalSessionId(record: WireRecord): string | undefined | null {
	if (!hasOwnWireField(record, 'session_id')) return undefined;
	return canonicalIdField(record, 'session_id', 'ses');
}

function requiredU32(record: WireRecord, field: string): number | null {
	return isU32(record[field]) ? record[field] : null;
}

function requiredSafeUnsignedInteger(record: WireRecord, field: string): number | null {
	return isSafeUnsignedInteger(record[field]) ? record[field] : null;
}

function requiredBoolean(record: WireRecord, field: string): boolean | null {
	return isBoolean(record[field]) ? record[field] : null;
}

function optionalU32IsValid(record: WireRecord, field: string): boolean {
	return record[field] === undefined || isU32(record[field]);
}

function optionalSafeUnsignedIntegerIsValid(record: WireRecord, field: string): boolean {
	return record[field] === undefined || isSafeUnsignedInteger(record[field]);
}

function nullableStringIsValid(value: unknown): boolean {
	return value === null || isString(value);
}

function nullableCostIsValid(value: unknown): boolean {
	return value === null || (isFiniteNumber(value) && value >= 0);
}

function nullableU32IsValid(value: unknown): boolean {
	return value === null || isU32(value);
}

function isCanonicalAssetIdArray(value: unknown): value is string[] {
	return Array.isArray(value) && value.every((assetId) => isCanonicalEntityId(assetId, 'asset'));
}

function isCanonicalSandboxReviewStepIdArray(value: unknown): value is string[] {
	return (
		Array.isArray(value) &&
		value.length <= 64 &&
		value.every((stepId) => isCanonicalEntityId(stepId, 'step'))
	);
}

function mapToolResult(value: unknown): AgentToolResultEnvelope | null {
	if (!isRecord(value)) return null;
	const outcome = readStringField(value, 'outcome');
	const idempotency = readStringField(value, 'idempotency');
	const retryability = readStringField(value, 'retryability');
	const errorClass = value.error_class;
	if (
		!isOneOf(outcome, TOOL_EXECUTION_OUTCOME_VALUES) ||
		!isOneOf(idempotency, OPERATION_IDEMPOTENCY_VALUES) ||
		!isOneOf(retryability, TOOL_RETRYABILITY_VALUES) ||
		(errorClass !== undefined &&
			errorClass !== null &&
			!isOneOf(errorClass, TOOL_ERROR_CLASS_VALUES)) ||
		!isCanonicalAssetIdArray(value.assets) ||
		(value.sandbox_output_review_step_ids !== undefined &&
			!isCanonicalSandboxReviewStepIdArray(value.sandbox_output_review_step_ids)) ||
		!['verification_hint', 'next_action'].every(
			(field) => value[field] === undefined || nullableStringIsValid(value[field]),
		)
	) {
		return null;
	}
	return {
		outcome,
		errorClass: errorClass as ToolErrorClass | null | undefined,
		idempotency,
		retryability,
		verificationHint: value.verification_hint as string | null | undefined,
		nextAction: value.next_action as string | null | undefined,
		assets: value.assets,
		sandboxOutputReviewStepIds: value.sandbox_output_review_step_ids as string[] | undefined,
	};
}

function mapCacheDiagnostics(value: unknown): CacheDiagnostics | null {
	if (
		!isRecord(value) ||
		!isOneOf(value.strategy, PROMPT_CACHE_STRATEGY_VALUES) ||
		!isString(value.provider) ||
		!isBoolean(value.key_requested) ||
		!isBoolean(value.system_split) ||
		!isBoolean(value.downgraded) ||
		!isOneOf(value.outcome, CACHE_DIAGNOSTIC_OUTCOME_VALUES) ||
		!isOneOf(value.usage_source, CACHE_USAGE_SOURCE_VALUES)
	) {
		return null;
	}
	return {
		strategy: value.strategy,
		provider: value.provider,
		key_requested: value.key_requested,
		system_split: value.system_split,
		downgraded: value.downgraded,
		outcome: value.outcome,
		usage_source: value.usage_source,
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
		!isString(event.event) ||
		!AGENT_EVENT_NAME_SET.has(event.event) ||
		!isValidTauriEventId(event.id) ||
		!isRecord(event.payload) ||
		!matchesGeneratedEventPayload(event.event, event.payload)
	) {
		return null;
	}

	const tauriEvent = event as unknown as TauriEvent<unknown>;
	const eventName = event.event;
	const payload = event.payload;
	switch (eventName) {
		case 'agent:thought': {
			const sessionId = canonicalIdField(payload, 'session_id', 'ses');
			const thought = readStringField(payload, 'thought');
			const stepNumber = requiredU32(payload, 'step_number');
			const runId = requiredSafeUnsignedInteger(payload, 'run_id');
			const messageId = canonicalIdField(payload, 'message_id', 'step');
			if (
				sessionId === null ||
				thought === null ||
				stepNumber === null ||
				runId === null ||
				messageId === null ||
				!optionalSafeUnsignedIntegerIsValid(payload, 'event_sequence')
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
					...(payload.event_sequence !== undefined
						? { eventSequence: payload.event_sequence as number }
						: {}),
				},
			};
		}
		case 'agent:tool_call': {
			const sessionId = canonicalIdField(payload, 'session_id', 'ses');
			const toolName = readStringField(payload, 'tool_name');
			const stepNumber = requiredU32(payload, 'step_number');
			const runId = requiredSafeUnsignedInteger(payload, 'run_id');
			const toolIndex = requiredU32(payload, 'tool_index');
			const stepId = canonicalIdField(payload, 'step_id', 'step');
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
				!hasOwnWireField(payload, 'input') ||
				(toolCallId !== null && !isString(toolCallId)) ||
				!optionalSafeUnsignedIntegerIsValid(payload, 'event_sequence')
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
					...(payload.event_sequence !== undefined
						? { eventSequence: payload.event_sequence as number }
						: {}),
				},
			};
		}
		case 'agent:observation': {
			const sessionId = canonicalIdField(payload, 'session_id', 'ses');
			const observation = readStringField(payload, 'observation');
			const toolName = readStringField(payload, 'tool_name');
			const stepNumber = requiredU32(payload, 'step_number');
			const runId = requiredSafeUnsignedInteger(payload, 'run_id');
			const silent = requiredBoolean(payload, 'silent');
			const toolIndex = requiredU32(payload, 'tool_index');
			const askOptions = isStringArray(payload.ask_options)
				? [...new Set(payload.ask_options.map((option) => option.trim()).filter(Boolean))]
				: null;
			const stepId = canonicalIdField(payload, 'step_id', 'step');
			const idempotency = payload.idempotency;
			const operationScope = payload.operation_scope;
			const renderer = readStringField(payload, 'renderer');
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
				(toolCallId !== null && !isString(toolCallId)) ||
				result === null ||
				!optionalSafeUnsignedIntegerIsValid(payload, 'event_sequence')
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
					...(payload.event_sequence !== undefined
						? { eventSequence: payload.event_sequence as number }
						: {}),
				},
			};
		}
		case 'agent:thought_chunk':
		case 'agent:reasoning_chunk': {
			const sessionId = canonicalIdField(payload, 'session_id', 'ses');
			const delta = readStringField(payload, 'delta');
			const stepNumber = requiredU32(payload, 'step_number');
			const runId = requiredSafeUnsignedInteger(payload, 'run_id');
			const messageId = canonicalIdField(payload, 'message_id', 'step');
			const seq = requiredSafeUnsignedInteger(payload, 'seq');
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
			const sessionId = canonicalIdField(payload, 'session_id', 'ses');
			const previewId = canonicalIdField(payload, 'preview_id', 'preview');
			const toolName = readStringField(payload, 'tool_name');
			const argumentsText = readStringField(payload, 'arguments');
			const argumentsTruncated = requiredBoolean(payload, 'arguments_truncated');
			const stepNumber = requiredU32(payload, 'step_number');
			const runId = requiredSafeUnsignedInteger(payload, 'run_id');
			const toolIndex = requiredU32(payload, 'tool_index');
			const seq = requiredSafeUnsignedInteger(payload, 'seq');
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
			const sessionId = canonicalIdField(payload, 'session_id', 'ses');
			const stepNumber = requiredU32(payload, 'step_number');
			const runId = requiredSafeUnsignedInteger(payload, 'run_id');
			const thoughtMessageId = canonicalIdField(payload, 'thought_message_id', 'step');
			const reasoningMessageId = canonicalIdField(payload, 'reasoning_message_id', 'msg');
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
			const sessionId = canonicalIdField(payload, 'session_id', 'ses');
			const phase = payload.phase;
			const stepNumber = requiredU32(payload, 'step_number');
			const runId = requiredSafeUnsignedInteger(payload, 'run_id');
			if (
				sessionId === null ||
				!isOneOf(phase, WEB_SEARCH_PHASE_VALUES) ||
				stepNumber === null ||
				runId === null ||
				!optionalStringFieldIsValid(payload, 'call_id') ||
				!optionalU32IsValid(payload, 'call_index') ||
				!optionalStringFieldIsValid(payload, 'action') ||
				(payload.call_id !== undefined &&
					payload.call_id !== '' &&
					payload.call_index !== undefined)
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
					...(payload.call_index !== undefined
						? { callIndex: payload.call_index as number }
						: {}),
					...(payload.action !== undefined ? { action: payload.action as string } : {}),
					...(payload.result !== undefined ? { result: payload.result } : {}),
				},
			};
		}
		case 'agent:stream_stalled': {
			const sessionId = canonicalIdField(payload, 'session_id', 'ses');
			return sessionId === null ? null : { ...tauriEvent, payload: { sessionId } };
		}
		case 'agent:media_plan': {
			const sessionId = canonicalIdField(payload, 'session_id', 'ses');
			const stepNumber = requiredU32(payload, 'step_number');
			const runId = requiredSafeUnsignedInteger(payload, 'run_id');
			const requestKind = payload.request_kind;
			const strategy = payload.strategy;
			const projections =
				Array.isArray(payload.projections) &&
				payload.projections.every(
					(item) =>
						isRecord(item) &&
						isCanonicalEntityId(item.asset_id, 'asset') &&
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
						isCanonicalEntityId(item.asset_id, 'asset') &&
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
				!isOneOf(requestKind, REQUEST_KIND_VALUES) ||
				!isOneOf(strategy, MEDIA_INPUT_STRATEGY_VALUES) ||
				projections === null ||
				notices === null ||
				!optionalSafeUnsignedIntegerIsValid(payload, 'event_sequence')
			)
				return null;
			return {
				...tauriEvent,
				payload: {
					sessionId,
					stepNumber,
					runId,
					requestKind,
					strategy,
					projections,
					notices,
					...(payload.event_sequence !== undefined
						? { eventSequence: payload.event_sequence as number }
						: {}),
				},
			};
		}
		case 'agent:supplement': {
			const sessionId = canonicalIdField(payload, 'session_id', 'ses');
			const additionalContext = readStringField(payload, 'additional_context');
			const stepNumber = requiredU32(payload, 'step_number');
			const runId = requiredSafeUnsignedInteger(payload, 'run_id');
			const supplementId = canonicalIdField(payload, 'supplement_id', 'msg');
			if (
				sessionId === null ||
				additionalContext === null ||
				stepNumber === null ||
				runId === null ||
				supplementId === null ||
				!optionalCanonicalIdFieldIsValid(payload, 'message_id', 'msg') ||
				(payload.inject_source !== undefined &&
					!isOneOf(payload.inject_source, INJECT_SOURCE_VALUES)) ||
				!optionalSafeUnsignedIntegerIsValid(payload, 'event_sequence')
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
						? { injectSource: payload.inject_source as InjectSource }
						: {}),
					...(payload.event_sequence !== undefined
						? { eventSequence: payload.event_sequence as number }
						: {}),
				},
			};
		}
		case 'agent:compaction': {
			const sessionId = canonicalIdField(payload, 'session_id', 'ses');
			const summary = readStringField(payload, 'summary');
			const tokensBefore = requiredU32(payload, 'tokens_before');
			const tokensAfter = requiredU32(payload, 'tokens_after');
			const degraded = requiredBoolean(payload, 'degraded');
			if (
				sessionId === null ||
				summary === null ||
				tokensBefore === null ||
				tokensAfter === null ||
				degraded === null ||
				!optionalCanonicalIdFieldIsValid(payload, 'episode_id', 'msg') ||
				!optionalSafeUnsignedIntegerIsValid(payload, 'event_sequence')
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
					...(payload.event_sequence !== undefined
						? { eventSequence: payload.event_sequence as number }
						: {}),
				},
			};
		}
		case 'agent:usage': {
			const sessionId = canonicalIdField(payload, 'session_id', 'ses');
			const cacheAccounting = payload.cache_accounting;
			const rawCacheDiagnostics = payload.cache_diagnostics;
			const cacheDiagnostics =
				rawCacheDiagnostics === undefined
					? undefined
					: mapCacheDiagnostics(rawCacheDiagnostics);
			const requestKind = payload.request_kind;
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
				(rawCacheDiagnostics !== undefined && cacheDiagnostics === null) ||
				!numberFields.every((field) => isU32(payload[field])) ||
				!isBoolean(payload.cache_exclusive) ||
				!isOneOf(cacheAccounting, CACHE_ACCOUNTING_VALUES) ||
				!nullableCostIsValid(payload.cost_usd) ||
				!nullableStringIsValid(payload.model) ||
				!nullableCostIsValid(payload.cumulative_cost_usd) ||
				!nullableU32IsValid(payload.context_window) ||
				!optionalU32IsValid(payload, 'step_number') ||
				!optionalSafeUnsignedIntegerIsValid(payload, 'duration_ms') ||
				(requestKind !== undefined && !isOneOf(requestKind, REQUEST_KIND_VALUES)) ||
				!isOneOf(payload.call_kind, LLM_CALL_KIND_VALUES) ||
				!isBoolean(payload.has_cost) ||
				payload.has_cost !== (payload.cost_usd !== null)
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
					...(cacheDiagnostics !== undefined ? { cacheDiagnostics } : {}),
					cumulativeCostUsd: payload.cumulative_cost_usd as number | null,
					contextWindow: payload.context_window as number | null,
					...(payload.step_number !== undefined
						? { stepNumber: payload.step_number as number }
						: {}),
					...(payload.duration_ms !== undefined
						? { durationMs: payload.duration_ms as number }
						: {}),
					...(requestKind !== undefined ? { requestKind } : {}),
					callKind: payload.call_kind,
					hasCost: payload.has_cost,
				},
			};
		}
		case 'agent:tool_output': {
			const sessionId = canonicalIdField(payload, 'session_id', 'ses');
			const stepId = canonicalIdField(payload, 'step_id', 'step');
			const output = readStringField(payload, 'output');
			return sessionId === null || stepId === null || output === null
				? null
				: { ...tauriEvent, payload: { sessionId, stepId, output } };
		}
		case 'notification:show': {
			const notificationKind = payload.notification_kind;
			if (
				notificationKind !== undefined &&
				!isOneOf(notificationKind, AGENT_NOTIFICATION_KIND_VALUES)
			)
				return null;
			if (notificationKind === 'tool_run_completion') {
				const sessionId = optionalSessionId(payload);
				const title = readStringField(payload, 'title');
				const body = readStringField(payload, 'body');
				const toolRunKind = payload.tool_run_kind;
				const toolRunId = canonicalIdField(payload, 'tool_run_id', 'toolrun');
				const toolRunStatus = payload.tool_run_status;
				if (
					toolRunStatus !== undefined &&
					!isOneOf(toolRunStatus, TOOL_RUN_COMPLETION_STATUS_DTO_VALUES)
				)
					return null;
				if (
					toolRunKind !== undefined &&
					!isOneOf(toolRunKind, TOOL_RUN_KIND_DTO_VALUES)
				)
					return null;
				if (
					sessionId === null ||
					title === null ||
					body === null ||
					toolRunKind !== 'background' ||
					toolRunId === null ||
					sessionId === undefined ||
					toolRunStatus === undefined
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
			const title = readStringField(payload, 'title');
			const body = readStringField(payload, 'body');
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
