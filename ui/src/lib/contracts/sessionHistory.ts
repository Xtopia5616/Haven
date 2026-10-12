/** Session history wire DTOs and validated renderer projections. */
import { mapInteractionRequest, type InteractionRequest } from './app.ts';
import type {
	LlmUsageRecord as GeneratedLlmUsageRecord,
	MediaAsset as GeneratedMediaAsset,
	MediaInput as GeneratedMediaInput,
	MediaRepresentation as GeneratedMediaRepresentation,
	MessageAttachment as GeneratedMessageAttachment,
	SessionResumeMessageDto as GeneratedSessionResumeMessage,
	RuntimeSessionListResponse as GeneratedSessionListResponse,
	SessionLineageResponse as GeneratedSessionLineageResponse,
	SessionRecordDto as GeneratedSessionRecord,
	SessionResumeResponse as GeneratedSessionResumeResponse,
	SessionStep as GeneratedSessionStep,
	SessionUsage as GeneratedSessionUsage,
	StepInfo as GeneratedStepInfo,
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

type SessionHistoryWireRow = TauriCommandResponse<'list_session_history'>[number];
export type SessionHistoryRow = Omit<SessionHistoryWireRow, 'session_id'> & { sessionId: string };
type RuntimeStepWire = GeneratedSessionListResponse['sessions'][number]['steps'][number];
type RuntimeSessionWire = GeneratedSessionListResponse['sessions'][number];
export type RuntimeSessionListResponse = Omit<GeneratedSessionListResponse, 'sessions'> & {
	sessions: Array<
		Omit<RuntimeSessionWire, 'session_id' | 'steps'> & {
			sessionId: string;
			steps: Array<Omit<RuntimeStepWire, 'step_id'> & { stepId: string }>;
		}
	>;
};
export type SessionLineageResponse = Omit<
	GeneratedSessionLineageResponse,
	'session_id' | 'parent_session_id' | 'parent' | 'children'
> & {
	sessionId: string;
	parentSessionId: string | null;
	parent: SessionHistoryRow | null;
	children: Array<{ session: SessionHistoryRow; parentSessionId: string }>;
};
export type SessionResumeMessage = Omit<
	GeneratedSessionResumeMessage,
	'message_id' | 'session_id' | 'attachments' | 'media_inputs'
> & {
	messageId: string;
	sessionId: string;
	attachments: Array<RendererMessageAttachment>;
	media_inputs?: Array<RendererMediaInput>;
};
export type SessionResumeStep = Omit<
	GeneratedSessionStep,
	'step_id' | 'session_id'
> & {
	stepId: string;
	sessionId: string;
};
export type SessionResumeUsage = GeneratedSessionUsage;

type RendererMediaAsset = Omit<GeneratedMediaAsset, 'asset_id'> & { assetId: string };
type RendererManagedFilePayload = {
	kind: 'managed_file_ref';
	value: Omit<
		Extract<GeneratedMediaRepresentation['payload'], { kind: 'managed_file_ref' }>['value'],
		'asset_id'
	> & { assetId: string };
};
type RendererMediaRepresentation = Omit<GeneratedMediaRepresentation, 'payload'> & {
	payload:
		| Exclude<GeneratedMediaRepresentation['payload'], { kind: 'managed_file_ref' }>
		| RendererManagedFilePayload;
};
type RendererMessageAttachment = Omit<
	GeneratedMessageAttachment,
	'asset_id' | 'representations'
> & {
	assetId?: string;
	representations?: RendererMediaRepresentation[];
};
type RendererMediaInput = Omit<GeneratedMediaInput, 'asset' | 'representations'> & {
	asset: RendererMediaAsset;
	representations: RendererMediaRepresentation[];
};

/** Resume usage rows keep Rust field names except entity identifiers. */
export type SessionLlmUsage = Omit<
	Partial<GeneratedLlmUsageRecord>,
	'usage_id' | 'session_id' | 'cost_usd'
> & {
	usageId?: string;
	sessionId?: string;
	cost_usd?: number | null;
};

/** Validated resume response with entity IDs mapped to the renderer contract. */
export type SessionResumeResponse = Omit<
	GeneratedSessionResumeResponse,
	'session' | 'messages' | 'steps' | 'interactions' | 'llm_usage'
> & {
	session: SessionHistoryRow;
	messages: SessionResumeMessage[];
	steps: SessionResumeStep[];
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
	return value.steps.every((step: unknown) => isRecord(step) && isU32(step.step_number));
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
		(value.step_number === null || isU32(value.step_number)) &&
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
			(step: unknown) => isRecord(step) && isU32(step.step_number) && isI32(step.tool_index),
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

function mapSessionRecord(value: GeneratedSessionRecord): SessionHistoryRow {
	const { session_id, ...rest } = value;
	return {
		...rest,
		sessionId: session_id,
	};
}

function mapRuntimeStep(
	value: GeneratedStepInfo,
): RuntimeSessionListResponse['sessions'][number]['steps'][number] {
	const { step_id, ...rest } = value;
	return {
		...rest,
		stepId: step_id,
	};
}

function mapRuntimeSession(
	value: GeneratedSessionListResponse['sessions'][number],
): RuntimeSessionListResponse['sessions'][number] {
	const { session_id, steps, ...rest } = value;
	return {
		...rest,
		sessionId: session_id,
		steps: steps.map(mapRuntimeStep),
	};
}

function mapMediaAsset(value: GeneratedMediaAsset): RendererMediaAsset {
	const { asset_id, ...rest } = value;
	return {
		...rest,
		assetId: asset_id,
	};
}

function mapMediaRepresentation(
	value: GeneratedMediaRepresentation,
): RendererMediaRepresentation {
	const payload = value.payload;
	let mappedPayload: RendererMediaRepresentation['payload'];
	switch (payload.kind) {
		case 'inline_data':
		case 'text':
		case 'structured':
			mappedPayload = payload;
			break;
		case 'managed_file_ref':
			const { asset_id, ...payloadRest } = payload.value;
			mappedPayload = {
				kind: payload.kind,
				value: { ...payloadRest, assetId: asset_id },
			};
			break;
	}
	return { ...value, payload: mappedPayload };
}

function mapMessageAttachment(
	value: GeneratedMessageAttachment,
	): RendererMessageAttachment {
	const { asset_id, representations, ...rest } = value;
	return {
		...rest,
		...(asset_id === undefined ? {} : { assetId: asset_id }),
		...(representations === undefined
			? {}
			: { representations: representations.map(mapMediaRepresentation) }),
	};
}

function mapMediaInput(value: GeneratedMediaInput): RendererMediaInput {
	const { asset, representations, ...rest } = value;
	return {
		...rest,
		asset: mapMediaAsset(asset),
		representations: representations.map(mapMediaRepresentation),
	};
}

function mapResumeMessage(value: GeneratedSessionResumeMessage): SessionResumeMessage {
	const { message_id, session_id, attachments, media_inputs, ...rest } = value;
	return {
		...rest,
		messageId: message_id,
		sessionId: session_id,
		attachments: attachments.map(mapMessageAttachment),
		...(media_inputs === undefined ? {} : { media_inputs: media_inputs.map(mapMediaInput) }),
	};
}

function mapResumeStep(value: GeneratedSessionStep): SessionResumeStep {
	const { step_id, session_id, ...rest } = value;
	return {
		...rest,
		stepId: step_id,
		sessionId: session_id,
	};
}

function mapLlmUsage(value: GeneratedLlmUsageRecord): SessionLlmUsage {
	const { usage_id, session_id, ...rest } = value;
	return {
		...rest,
		usageId: usage_id,
		sessionId: session_id,
	};
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
	const wire = value as unknown as GeneratedSessionListResponse;
	return {
		...wire,
		sessions: wire.sessions.map(mapRuntimeSession),
	};
}

/** Validate persisted session identities returned by history and lineage commands. */
export function parseSessionHistoryRows(value: unknown): SessionHistoryRow[] {
	if (!Array.isArray(value) || !value.every(hasSessionIdentity)) {
		throw new Error('Invalid session history identity');
	}
	for (const row of value)
		requireWireShape(row, SESSION_RECORD_RESPONSE_SHAPE, 'session history');
	return (value as unknown as SessionHistoryWireRow[]).map(mapSessionRecord);
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
	const wire = value as unknown as GeneratedSessionLineageResponse;
	const { session_id, parent_session_id, parent, children, ...rest } = wire;
	return {
		...rest,
		sessionId: session_id,
		parentSessionId: parent_session_id,
		parent: parent ? mapSessionRecord(parent) : null,
		children: children.map((child) => {
			const { session, parent_session_id, ...childRest } = child;
			return {
				...childRest,
				session: mapSessionRecord(session),
				parentSessionId: parent_session_id,
			};
		}),
	};
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
	const wire = value as unknown as GeneratedSessionResumeResponse;
	const {
		session,
		messages: wireMessages,
		steps: wireSteps,
		usage,
		llm_usage,
		interactions: _interactions,
		...rest
	} = wire;
	return {
		...rest,
		session: mapSessionRecord(session),
		messages: wireMessages.map(mapResumeMessage),
		steps: wireSteps.map(mapResumeStep),
		usage,
		llm_usage: llm_usage.map(mapLlmUsage),
		interactions,
	};
}
