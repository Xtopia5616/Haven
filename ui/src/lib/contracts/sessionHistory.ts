/** Session history and resume command DTOs in their Rust/Tauri wire shape. */
import type {
	LlmUsageRecord as GeneratedLlmUsageRecord,
	SessionResumeMessageDto as GeneratedSessionResumeMessage,
	RuntimeSessionListResponse as GeneratedSessionListResponse,
	SessionResumeResponse as GeneratedSessionResumeResponse,
	SessionStep as GeneratedSessionStep,
	SessionUsage as GeneratedSessionUsage,
	TauriCommandResponse,
} from './generatedCommands.ts';
import { INTERACTION_KIND_VALUES } from './generatedCommands.ts';
import { isRecord } from './objectGuards.ts';
import { isOneOf } from './valueGuards.ts';
import { isCanonicalEntityId } from './wireGuards.ts';

/** All command wire shapes are derived from Rust handlers and DTOs. */
export type SessionHistoryRow = TauriCommandResponse<'list_session_history'>[number];
export type RuntimeSessionListResponse = GeneratedSessionListResponse;
export type SessionLineageResponse = TauriCommandResponse<'get_session_lineage'>;
export type SessionResumeMessage = GeneratedSessionResumeMessage;
export type SessionResumeStep = GeneratedSessionStep;
export type SessionResumeUsage = GeneratedSessionUsage;

/** Renderer usage projection derives its fields from the Rust usage DTO. */
export type SessionLlmUsage = Omit<Partial<GeneratedLlmUsageRecord>, 'cost_usd'> & {
	cost_usd?: number | null;
};

/** Complete successful response from get_session_for_resume/get_latest_session_for_resume. */
export type SessionResumeResponse = GeneratedSessionResumeResponse;

function isSessionRecord(
	value: unknown,
): value is Record<string, unknown> & { session_id: string } {
	return isRecord(value) && isCanonicalEntityId(value.session_id, 'ses');
}

function hasCanonicalSessionId(
	value: unknown,
): value is Record<string, unknown> & { session_id: string } {
	return isRecord(value) && isCanonicalEntityId(value.session_id, 'ses');
}

function isRuntimeSession(value: unknown): boolean {
	return (
		isRecord(value) &&
		isCanonicalEntityId(value.session_id, 'ses') &&
		Array.isArray(value.steps) &&
		value.steps.every(
			(step: unknown) => isRecord(step) && isCanonicalEntityId(step.step_id, 'step'),
		)
	);
}

/** Validate the session and step identities before a runtime list reaches the reducer. */
export function parseRuntimeSessionListResponse(value: unknown): RuntimeSessionListResponse {
	if (
		!isRecord(value) ||
		!Array.isArray(value.sessions) ||
		!value.sessions.every(isRuntimeSession)
	) {
		throw new Error('Invalid runtime session identity');
	}
	return value as unknown as RuntimeSessionListResponse;
}

/** Validate persisted session identities returned by history and lineage commands. */
export function parseSessionHistoryRows(value: unknown): SessionHistoryRow[] {
	if (!Array.isArray(value) || !value.every(isSessionRecord)) {
		throw new Error('Invalid session history identity');
	}
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
			(isSessionRecord(value.parent) &&
				value.parent.session_id === value.parent_session_id)
		) ||
		!Array.isArray(value.children) ||
		!value.children.every(
			(child: unknown) =>
				isRecord(child) &&
				isSessionRecord(child.session) &&
				isCanonicalEntityId(child.parent_session_id, 'ses') &&
				child.parent_session_id === expectedSessionId &&
				child.session.session_id !== expectedSessionId,
		)
	) {
		throw new Error('Invalid session lineage identity');
	}
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
	if (!isRecord(value) || !isSessionRecord(value.session)) {
		throw new Error('Invalid resumed session identity');
	}
	const sessionId = value.session.session_id;
	if (expectedSessionId !== undefined && expectedSessionId !== sessionId) {
		throw new Error('Invalid resumed session identity');
	}
	if (
		!Array.isArray(value.steps) ||
		!value.steps.every(
			(step: unknown) => hasCanonicalSessionId(step) && step.session_id === sessionId,
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
			.filter(
				(step: unknown) => isRecord(step) && isCanonicalEntityId(step.step_id, 'step'),
			)
			.map((step: { step_id: string }) => step.step_id),
	);
	const supplementStepMessageIds = new Set<string>(
		value.steps
			.filter(
				(step: unknown) => isRecord(step) && isCanonicalEntityId(step.step_id, 'msg'),
			)
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
	return value as unknown as SessionResumeResponse;
}
