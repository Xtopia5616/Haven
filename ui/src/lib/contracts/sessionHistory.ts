/** Session history wire DTOs and validated renderer projections. */
import { mapInteractionRequest, type InteractionRequest } from './app.ts';
import type {
	LlmUsageRecord as GeneratedLlmUsageRecord,
	SessionResumeMessageDto as GeneratedSessionResumeMessage,
	RuntimeSessionListResponse as GeneratedSessionListResponse,
	SessionResumeResponse as GeneratedSessionResumeResponse,
	SessionStep as GeneratedSessionStep,
	SessionUsage as GeneratedSessionUsage,
	TauriCommandResponse,
} from './generatedCommands.ts';
import {
	INTERACTION_KIND_VALUES,
	RUNTIME_SESSION_LIST_RESPONSE_SHAPE,
	SESSION_LINEAGE_RESPONSE_SHAPE,
	SESSION_RECORD_RESPONSE_SHAPE,
	SESSION_RESUME_RESPONSE_SHAPE,
	REQUEST_KIND_VALUES,
} from './generatedCommands.ts';
import { findGeneratedWireShapeMismatch } from './generatedWireShape.ts';
import { isRecord } from './objectGuards.ts';
import {
	isFiniteNumber,
	isI32,
	isNonNegativeFiniteNumber,
	isOneOf,
	isSafeUnsignedInteger,
	isU32,
} from './valueGuards.ts';
import { isCanonicalEntityId } from './wireGuards.ts';

/** All command wire shapes are derived from Rust handlers and DTOs. */
export type SessionHistoryRow = TauriCommandResponse<'list_session_history'>[number];
export type RuntimeSessionListResponse = GeneratedSessionListResponse;
export type SessionLineageResponse = TauriCommandResponse<'get_session_lineage'>;
export type SessionResumeMessage = GeneratedSessionResumeMessage;
export type SessionResumeStep = GeneratedSessionStep;
export type SessionResumeUsage = GeneratedSessionUsage;

/** Resume usage rows retain Rust field names while refining nullable cost data. */
export type SessionLlmUsage = Omit<
	Partial<GeneratedLlmUsageRecord>,
	'cost_usd'
> & {
	cost_usd?: number | null;
};

/** Validated resume response with interactions mapped to the renderer contract. */
export type SessionResumeResponse = Omit<
	GeneratedSessionResumeResponse,
	'interactions' | 'llm_usage'
> & {
	interactions: InteractionRequest[];
	llm_usage: SessionLlmUsage[];
};

function hasSessionIdentity(
	value: unknown,
): value is Record<string, unknown> & { session_id: string } {
	return isRecord(value) && isCanonicalEntityId(value.session_id, 'ses');
}

function hasRuntimeSessionIdentity(value: unknown): boolean {
	return (
		isRecord(value) &&
		isCanonicalEntityId(value.session_id, 'ses') &&
		Array.isArray(value.steps) &&
		value.steps.every(
			(step: unknown) => isRecord(step) && isCanonicalEntityId(step.step_id, 'step'),
		)
	);
}

function requireWireShape(
	value: unknown,
	shape: Parameters<typeof findGeneratedWireShapeMismatch>[1],
	label: string,
): void {
	const mismatch = findGeneratedWireShapeMismatch(value, shape);
	if (mismatch) throw new Error(`Invalid ${label} response shape at ${mismatch}`);
}

function hasOptionalU32(record: Record<string, unknown>, field: string): boolean {
	return record[field] === undefined || isU32(record[field]);
}

function hasOptionalSafeUnsignedInteger(record: Record<string, unknown>, field: string): boolean {
	return record[field] === undefined || isSafeUnsignedInteger(record[field]);
}

function hasValidRuntimeSessionValues(value: unknown): boolean {
	if (!isRecord(value) || !Array.isArray(value.steps)) return false;
	if (value.waiting_reason !== undefined && value.status !== 'paused') return false;
	return value.steps.every((step: unknown) => isRecord(step) && isI32(step.step_number));
}

function hasValidRepresentationNumericValues(value: unknown): boolean {
	if (!isRecord(value)) return false;
	if (value.confidence !== undefined && !isFiniteNumber(value.confidence)) return false;
	if (value.cost === undefined) return true;
	if (!isRecord(value.cost)) return false;
	return (
		hasOptionalU32(value.cost, 'estimated_tokens') &&
		(value.cost.estimated_usd === undefined || isFiniteNumber(value.cost.estimated_usd))
	);
}

function hasValidMessageNumericValues(value: Record<string, unknown>): boolean {
	if (!Array.isArray(value.attachments)) return false;
	const validAttachments = value.attachments.every((attachment: unknown) => {
		if (!isRecord(attachment) || !hasOptionalSafeUnsignedInteger(attachment, 'size_bytes')) {
			return false;
		}
		return (
			attachment.representations === undefined ||
			(Array.isArray(attachment.representations) &&
				attachment.representations.every(hasValidRepresentationNumericValues))
		);
	});
	if (!validAttachments || value.media_inputs === undefined) return validAttachments;
	return (
		Array.isArray(value.media_inputs) &&
		value.media_inputs.every(
			(input: unknown) =>
				isRecord(input) &&
				isRecord(input.asset) &&
				isSafeUnsignedInteger(input.asset.size_bytes) &&
				Array.isArray(input.representations) &&
				input.representations.every(hasValidRepresentationNumericValues),
		)
	);
}

function hasValidSessionUsageNumericValues(value: unknown): boolean {
	if (value === null) return true;
	if (!isRecord(value)) return false;
	return (
		[
			'prompt_tokens',
			'completion_tokens',
			'total_tokens',
			'cached_tokens',
			'cache_creation_tokens',
			'cache_miss_tokens',
			'context_tokens',
		].every((field) => isU32(value[field])) &&
		(value.context_window === null || isU32(value.context_window)) &&
		isNonNegativeFiniteNumber(value.cost_usd)
	);
}

function hasValidLlmUsageNumericValues(value: unknown): boolean {
	if (!isRecord(value)) return false;
	return (
		isOneOf(value.request_kind, REQUEST_KIND_VALUES) &&
		(value.step_number === null || isI32(value.step_number)) &&
		[
			'prompt_tokens',
			'completion_tokens',
			'total_tokens',
			'cached_tokens',
			'cache_creation_tokens',
			'cache_miss_tokens',
			'context_tokens',
		].every((field) => isU32(value[field])) &&
		(value.context_window === null || isU32(value.context_window)) &&
		isNonNegativeFiniteNumber(value.cost_usd) &&
		(value.duration_ms === null || isSafeUnsignedInteger(value.duration_ms))
	);
}

function hasValidResumeNumericValues(value: Record<string, unknown>): boolean {
	if (
		!Array.isArray(value.steps) ||
		!Array.isArray(value.messages) ||
		!Array.isArray(value.llm_usage)
	) {
		return false;
	}
	return (
		value.steps.every(
			(step: unknown) => isRecord(step) && isI32(step.step_number) && isI32(step.tool_index),
		) &&
		value.messages.every(
			(message: unknown) => isRecord(message) && hasValidMessageNumericValues(message),
		) &&
		hasValidSessionUsageNumericValues(value.usage) &&
		value.llm_usage.every(hasValidLlmUsageNumericValues) &&
		Array.isArray(value.interactions) &&
		value.interactions.every(
			(interaction: unknown) =>
				isRecord(interaction) && hasOptionalU32(interaction, 'tool_index'),
		)
	);
}

/** Validate the session and step identities before a runtime list reaches the reducer. */
export function parseRuntimeSessionListResponse(value: unknown): RuntimeSessionListResponse {
	if (
		!isRecord(value) ||
		!Array.isArray(value.sessions) ||
		!value.sessions.every(hasRuntimeSessionIdentity)
	) {
		throw new Error('Invalid runtime session identity');
	}
	requireWireShape(value, RUNTIME_SESSION_LIST_RESPONSE_SHAPE, 'runtime session list');
	if (!value.sessions.every(hasValidRuntimeSessionValues)) {
		throw new Error('Invalid runtime session response value');
	}
	return value as unknown as RuntimeSessionListResponse;
}

/** Validate persisted session identities returned by history and lineage commands. */
export function parseSessionHistoryRows(value: unknown): SessionHistoryRow[] {
	if (!Array.isArray(value) || !value.every(hasSessionIdentity)) {
		throw new Error('Invalid session history identity');
	}
	for (const row of value)
		requireWireShape(row, SESSION_RECORD_RESPONSE_SHAPE, 'session history');
	return value as unknown as SessionHistoryRow[];
}

export function parseSessionLineageResponse(
	value: unknown,
	expectedSessionId: string,
): SessionLineageResponse {
	if (
		!isRecord(value) ||
		!isCanonicalEntityId(expectedSessionId, 'ses') ||
		!isCanonicalEntityId(value.session_id, 'ses') ||
		value.session_id !== expectedSessionId ||
		!(
			value.parent_session_id === null || isCanonicalEntityId(value.parent_session_id, 'ses')
		) ||
		!(
			value.parent === null ||
			(hasSessionIdentity(value.parent) &&
				value.parent.session_id === value.parent_session_id)
		) ||
		!Array.isArray(value.children) ||
		!value.children.every(
			(child: unknown) =>
				isRecord(child) &&
				hasSessionIdentity(child.session) &&
				isCanonicalEntityId(child.parent_session_id, 'ses') &&
				child.parent_session_id === expectedSessionId &&
				child.session.session_id !== expectedSessionId,
		)
	) {
		throw new Error('Invalid session lineage identity');
	}
	requireWireShape(value, SESSION_LINEAGE_RESPONSE_SHAPE, 'session lineage');
	return value as unknown as SessionLineageResponse;
}

function hasResumeInteractionIdentity(
	value: unknown,
	sessionId: string,
	stepIds: ReadonlySet<string>,
): boolean {
	if (
		!isRecord(value) ||
		!isRecord(value.owner) ||
		!isOneOf(value.kind, INTERACTION_KIND_VALUES)
	) {
		return false;
	}
	const expectedIdPrefix = value.kind === 'ask' ? 'step' : 'conf';
	if (!isCanonicalEntityId(value.request_id, expectedIdPrefix)) return false;
	if (value.kind === 'ask' && !stepIds.has(value.request_id)) return false;
	if (value.session_id !== undefined && value.session_id !== sessionId) return false;
	if (value.session_id !== undefined && !isCanonicalEntityId(value.session_id, 'ses')) {
		return false;
	}
	if (
		value.invocation_step_id !== undefined &&
		(!isCanonicalEntityId(value.invocation_step_id, 'step') ||
			!stepIds.has(value.invocation_step_id))
	) {
		return false;
	}
	return value.owner.kind === 'session' && value.owner.session_id === sessionId;
}

function hasValidMediaRepresentationAssetIdentity(value: unknown): boolean {
	if (!isRecord(value) || !isRecord(value.payload)) return false;
	if (value.payload.kind !== 'managed_file_ref') return true;
	return (
		isRecord(value.payload.value) && isCanonicalEntityId(value.payload.value.asset_id, 'asset')
	);
}

function hasValidOptionalRepresentations(value: unknown): boolean {
	return (
		value === undefined ||
		(Array.isArray(value) && value.every(hasValidMediaRepresentationAssetIdentity))
	);
}

function hasValidMessageAssetIdentities(message: Record<string, unknown>): boolean {
	if (
		!Array.isArray(message.attachments) ||
		!message.attachments.every(
			(attachment: unknown) =>
				isRecord(attachment) &&
				(attachment.asset_id === undefined ||
					isCanonicalEntityId(attachment.asset_id, 'asset')) &&
				hasValidOptionalRepresentations(attachment.representations),
		)
	) {
		return false;
	}
	if (message.media_inputs === undefined) return true;
	return (
		Array.isArray(message.media_inputs) &&
		message.media_inputs.every(
			(input: unknown) =>
				isRecord(input) &&
				isRecord(input.asset) &&
				isCanonicalEntityId(input.asset.asset_id, 'asset') &&
				Array.isArray(input.representations) &&
				input.representations.every(hasValidMediaRepresentationAssetIdentity),
		)
	);
}

/** Validate durable row identities before resume/replay data enters UI state. */
export function parseSessionResumeResponse(
	value: unknown,
	expectedSessionId?: string,
): SessionResumeResponse {
	if (!isRecord(value) || !hasSessionIdentity(value.session)) {
		throw new Error('Invalid resumed session identity');
	}
	const sessionId = value.session.session_id;
	if (expectedSessionId !== undefined && expectedSessionId !== sessionId) {
		throw new Error('Invalid resumed session identity');
	}
	if (
		!Array.isArray(value.steps) ||
		!value.steps.every(
			(step: unknown) => hasSessionIdentity(step) && step.session_id === sessionId,
		)
	) {
		throw new Error('Invalid resumed step session identity');
	}
	if (
		!value.steps.every(
			(step: unknown) =>
				isRecord(step) &&
				(isCanonicalEntityId(step.step_id, 'step') ||
					isCanonicalEntityId(step.step_id, 'msg')),
		)
	) {
		throw new Error('Invalid resumed step identity');
	}
	const stepIds = new Set<string>(
		value.steps
			.filter((step: unknown) => isRecord(step) && isCanonicalEntityId(step.step_id, 'step'))
			.map((step: { step_id: string }) => step.step_id),
	);
	const supplementStepMessageIds = new Set<string>(
		value.steps
			.filter((step: unknown) => isRecord(step) && isCanonicalEntityId(step.step_id, 'msg'))
			.map((step: { step_id: string }) => step.step_id),
	);
	const validResumeMessage = (
		message: unknown,
	): message is Record<string, unknown> & { message_id: string } =>
		isRecord(message) &&
		isCanonicalEntityId(message.session_id, 'ses') &&
		message.session_id === sessionId &&
		(isCanonicalEntityId(message.message_id, 'msg') ||
			(isCanonicalEntityId(message.message_id, 'step') && stepIds.has(message.message_id))) &&
		hasValidMessageAssetIdentities(message);
	const messages = value.messages;
	if (
		!Array.isArray(messages) ||
		!messages.every(validResumeMessage) ||
		![...supplementStepMessageIds].every((id) =>
			messages.some((message: unknown) => isRecord(message) && message.message_id === id),
		)
	) {
		throw new Error('Invalid resumed message identity');
	}
	if (
		!Array.isArray(value.llm_usage) ||
		!value.llm_usage.every(
			(row: unknown) =>
				isRecord(row) &&
				isCanonicalEntityId(row.session_id, 'ses') &&
				row.session_id === sessionId &&
				isCanonicalEntityId(row.usage_id, 'usage'),
		)
	) {
		throw new Error('Invalid resumed usage identity');
	}
	if (
		!Array.isArray(value.interactions) ||
		!value.interactions.every((interaction: unknown) =>
			hasResumeInteractionIdentity(interaction, sessionId, stepIds),
		)
	) {
		throw new Error('Invalid resumed interaction identity');
	}
	requireWireShape(value, SESSION_RESUME_RESPONSE_SHAPE, 'session resume');
	if (!hasValidResumeNumericValues(value)) {
		throw new Error('Invalid resumed numeric value');
	}
	// A resume snapshot can contain a pending confirmation without a usable
	// deadline; as before, omit that interaction while keeping the transcript.
	const interactions = value.interactions
		.map(mapInteractionRequest)
		.filter((interaction): interaction is InteractionRequest => interaction !== null);
	const llmUsage = value.llm_usage as SessionLlmUsage[];
	return { ...value, llm_usage: llmUsage, interactions } as unknown as SessionResumeResponse;
}
