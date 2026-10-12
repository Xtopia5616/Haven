import { describe, expect, it } from 'vitest';
import {
	parseRuntimeSessionListResponse,
	parseSessionHistoryRows,
	parseSessionLineageResponse,
	parseSessionResumeResponse,
} from './sessionHistory.ts';

const sessionId = 'ses-00000000000000000000000000000001';
const messageId = 'msg-00000000000000000000000000000002';
const stepId = 'step-00000000000000000000000000000003';
const usageId = 'usage-00000000000000000000000000000004';

type ResumeMediaRepresentationFixture = {
	representation: 'managed_file_ref';
	provenance: { kind: 'original' };
	availability: { state: 'available' };
	payload: {
		kind: 'managed_file_ref';
		value: { asset_id: string; filename: string | null };
	};
	confidence?: number;
	cost?: { estimated_tokens?: number; estimated_usd?: number };
};
type ResumeMessageFixture = Record<string, unknown> & {
	message_id: string;
	session_id: string;
	attachments: Array<
		Record<string, unknown> & {
			asset_id?: string;
			representations?: ResumeMediaRepresentationFixture[];
		}
	>;
	media_inputs?: Array<{
		asset: { asset_id: string; size_bytes: number; [key: string]: unknown };
		representations: ResumeMediaRepresentationFixture[];
		[key: string]: unknown;
	}>;
};
type ResumeFixture = Record<string, unknown> & {
	session: Record<string, unknown> & { session_id: string };
	messages: ResumeMessageFixture[];
	steps: Array<Record<string, unknown> & { step_id: string; session_id: string }>;
	llm_usage: Array<Record<string, unknown> & { usage_id: string; session_id: string }>;
	interactions: Array<Record<string, unknown> & { request_id: string }>;
};

function sessionRecord(id = sessionId): Record<string, unknown> & { session_id: string } {
	return {
		session_id: id,
		input_text: 'Open the project',
		title: null,
		status: 'completed',
		created_at: '2026-10-12T08:00:00Z',
		updated_at: '2026-10-12T08:01:00Z',
	};
}

function runtimeSession(): Record<string, unknown> & {
	session_id: string;
	status: string;
	steps: Array<Record<string, unknown> & { step_number: number }>;
} {
	return {
		session_id: sessionId,
		input: 'Open the project',
		summary: 'Open project files',
		title: null,
		status: 'running',
		steps: [
			{
				step_id: stepId,
				step_number: 1,
				tool_name: 'read_file',
				input: { path: 'README.md' },
				output: null,
				status: 'running',
				risk_level: 'safe',
				confirmed: null,
			},
		],
		created_at: '2026-10-12T08:00:00Z',
		updated_at: '2026-10-12T08:00:01Z',
	};
}

function managedRepresentation(assetId: string): ResumeMediaRepresentationFixture {
	return {
		representation: 'managed_file_ref',
		provenance: { kind: 'original' },
		confidence: 0.9,
		cost: { estimated_tokens: 16, estimated_usd: 0.0001 },
		availability: { state: 'available' },
		payload: {
			kind: 'managed_file_ref',
			value: { asset_id: assetId, filename: 'photo.png' },
		},
	};
}

function resumeResponse(): ResumeFixture {
	return {
		session: sessionRecord(),
		messages: [
			{
				message_id: messageId,
				session_id: sessionId,
				role: 'user',
				content: 'Open the project',
				message_type: null,
				created_at: '2026-10-12T08:00:00Z',
				tool_call_id: null,
				attachments: [],
				voice: false,
			},
			{
				message_id: stepId,
				session_id: sessionId,
				role: 'assistant',
				content: 'Reading the project instructions.',
				message_type: 'thought',
				created_at: '2026-10-12T08:00:01Z',
				tool_call_id: 'provider-call/opaque',
				attachments: [],
				voice: false,
			},
		],
		steps: [
			{
				step_id: stepId,
				session_id: sessionId,
				step_number: 1,
				tool_index: 0,
				thought: null,
				tool_name: 'read_file',
				tool_input: '{"path":"README.md"}',
				tool_call_id: 'another-external-id',
				observation: null,
				status: 'completed',
				is_high_risk: false,
				confirmed: null,
				silent: false,
				started_at: '2026-10-12T08:00:01Z',
				completed_at: '2026-10-12T08:00:02Z',
				created_at: '2026-10-12T08:00:01Z',
			},
		],
		usage: {
			prompt_tokens: 100,
			completion_tokens: 40,
			total_tokens: 140,
			cached_tokens: 0,
			cache_creation_tokens: 0,
			cache_miss_tokens: 0,
			context_tokens: 100,
			context_window: 128_000,
			cost_usd: 0.001,
			has_cost: true,
		},
		llm_usage: [
			{
				usage_id: usageId,
				session_id: sessionId,
				step_number: 1,
				role: 'chat',
				call_kind: 'agent',
				model: 'example-model',
				prompt_tokens: 100,
				completion_tokens: 40,
				total_tokens: 140,
				cached_tokens: 0,
				cache_creation_tokens: 0,
				cache_miss_tokens: 0,
				cache_accounting: 'unknown',
				cache_diagnostics: {
					strategy: 'off',
					provider: 'example-provider',
					key_requested: false,
					system_split: false,
					downgraded: false,
					outcome: 'unknown',
					usage_source: 'unavailable',
				},
				context_tokens: 100,
				context_window: 128_000,
				cost_usd: 0.001,
				has_cost: true,
				duration_ms: 250,
				created_at: '2026-10-12T08:00:01Z',
			},
		],
		interactions: [
			{
				request_id: stepId,
				session_id: sessionId,
				owner: { kind: 'session', session_id: sessionId },
				kind: 'ask',
				status: 'pending',
				created_at: '2026-10-12T08:00:03Z',
			},
			{
				request_id: 'conf-00000000000000000000000000000005',
				session_id: sessionId,
				owner: { kind: 'session', session_id: sessionId },
				kind: 'confirm',
				status: 'pending',
				invocation_step_id: stepId,
				tool_index: 0,
				risk_level: 'low',
				created_at: '2026-10-12T08:00:04Z',
			},
		],
	} as ResumeFixture;
}

function resumeResponseWithAssets(): ResumeFixture {
	const response = resumeResponse();
	response.steps.push({
		step_id: messageId,
		session_id: sessionId,
		step_number: 2,
		tool_index: 0,
		thought: null,
		tool_name: null,
		tool_input: null,
		tool_call_id: null,
		observation: null,
		status: 'completed',
		is_high_risk: false,
		confirmed: null,
		silent: false,
		started_at: null,
		completed_at: null,
		created_at: '2026-10-12T08:00:05Z',
	});
	response.messages = [
		{
			message_id: messageId,
			session_id: sessionId,
			role: 'user',
			content: 'Describe this image',
			message_type: null,
			created_at: '2026-10-12T08:00:00Z',
			tool_call_id: null,
			attachments: [
				{
					asset_id: 'asset-00000000000000000000000000000009',
					media_type: 'image/png',
					data: 'base64-data',
					size_bytes: 2048,
					representations: [
						managedRepresentation('asset-00000000000000000000000000000010'),
					],
				},
			],
			media_inputs: [
				{
					asset: {
						asset_id: 'asset-00000000000000000000000000000011',
						content_hash: 'abc123',
						media_type: 'image/png',
						size_bytes: 4096,
						source: 'user_attachment',
						lifecycle: 'session',
					},
					representations: [
						managedRepresentation('asset-00000000000000000000000000000012'),
					],
				},
			],
			voice: false,
		},
	];
	return response;
}

function sessionLineage() {
	return {
		session_id: sessionId,
		parent_session_id: null,
		parent: null,
		children: [
			{
				session: sessionRecord('ses-00000000000000000000000000000006'),
				parent_session_id: sessionId,
			},
		],
	};
}

describe('session history and resume contracts', () => {
	it('validates complete runtime sessions, closed enums, and canonical IDs', () => {
		expect(
			parseRuntimeSessionListResponse({ sessions: [runtimeSession()] }).sessions,
		).toHaveLength(1);

		const missingField = runtimeSession();
		delete (missingField.steps[0] as Record<string, unknown>).risk_level;
		expect(() => parseRuntimeSessionListResponse({ sessions: [missingField] })).toThrow(
			'Invalid runtime session list response shape',
		);

		const unknownStatus = runtimeSession();
		unknownStatus.status = 'future_status';
		expect(() => parseRuntimeSessionListResponse({ sessions: [unknownStatus] })).toThrow(
			'Invalid runtime session list response shape',
		);

		const badId = runtimeSession();
		badId.session_id = 'ses-short';
		expect(() => parseRuntimeSessionListResponse({ sessions: [badId] })).toThrow(
			'Invalid runtime session identity',
		);
	});

	it('checks i32 runtime values and waiting reason semantics', () => {
		const outsideI32 = runtimeSession();
		outsideI32.steps[0]!.step_number = 0x8000_0000;
		expect(() => parseRuntimeSessionListResponse({ sessions: [outsideI32] })).toThrow(
			'Invalid runtime session response value',
		);

		const waitingWhileRunning = runtimeSession();
		waitingWhileRunning.waiting_reason = 'ask';
		expect(() => parseRuntimeSessionListResponse({ sessions: [waitingWhileRunning] })).toThrow(
			'Invalid runtime session response value',
		);

		const paused = runtimeSession();
		paused.status = 'paused';
		paused.waiting_reason = 'ask';
		expect(parseRuntimeSessionListResponse({ sessions: [paused] }).sessions).toHaveLength(1);
	});

	it('validates complete persisted session rows and lineage rows', () => {
		expect(parseSessionHistoryRows([sessionRecord()])).toHaveLength(1);
		expect(() =>
			parseSessionHistoryRows([{ ...sessionRecord(), session_id: 'msg-invalid' }]),
		).toThrow('Invalid session history identity');
		expect(() =>
			parseSessionHistoryRows([{ ...sessionRecord(), input_text: undefined }]),
		).toThrow('Invalid session history response shape');

		expect(parseSessionLineageResponse(sessionLineage(), sessionId).children).toHaveLength(1);
		const malformedLineage = sessionLineage();
		(malformedLineage.children[0]!.session as Record<string, unknown>).status = 'unknown';
		expect(() => parseSessionLineageResponse(malformedLineage, sessionId)).toThrow(
			'Invalid session lineage response shape',
		);
	});

	it('checks lineage parent and direct-child relationship anchors', () => {
		expect(() =>
			parseSessionLineageResponse(
				{
					session_id: sessionId,
					parent_session_id: 'ses-00000000000000000000000000000040',
					parent: sessionRecord('ses-00000000000000000000000000000041'),
					children: [],
				},
				sessionId,
			),
		).toThrow('Invalid session lineage identity');

		const wrongChild = sessionLineage();
		wrongChild.children[0]!.parent_session_id = 'ses-00000000000000000000000000000043';
		expect(() => parseSessionLineageResponse(wrongChild, sessionId)).toThrow(
			'Invalid session lineage identity',
		);
	});

	it('accepts complete resume rows with canonical linkage and opaque provider IDs', () => {
		const response = resumeResponse();
		expect(parseSessionResumeResponse(response)).toBe(response);
		expect(parseSessionResumeResponse(response, sessionId)).toBe(response);
	});

	it('rejects missing fields, invalid enums, and malformed nested media shapes', () => {
		const missingMessageField = resumeResponse();
		delete missingMessageField.messages[0]!.voice;
		expect(() => parseSessionResumeResponse(missingMessageField)).toThrow(
			'Invalid session resume response shape',
		);

		const invalidRole = resumeResponse();
		invalidRole.messages[0]!.role = 'operator';
		expect(() => parseSessionResumeResponse(invalidRole)).toThrow(
			'Invalid session resume response shape',
		);

		const invalidCacheAccounting = resumeResponse();
		invalidCacheAccounting.llm_usage[0]!.cache_accounting = 'inclusive-ish';
		expect(() => parseSessionResumeResponse(invalidCacheAccounting)).toThrow(
			'Invalid session resume response shape',
		);

		const invalidCacheDiagnostics = resumeResponse();
		(
			invalidCacheDiagnostics.llm_usage[0]!.cache_diagnostics as Record<string, unknown>
		).strategy = 'future_strategy';
		expect(() => parseSessionResumeResponse(invalidCacheDiagnostics)).toThrow(
			'Invalid session resume response shape',
		);

		const invalidInteraction = resumeResponse();
		invalidInteraction.interactions[1]!.status = 'waiting';
		expect(() => parseSessionResumeResponse(invalidInteraction)).toThrow(
			'Invalid session resume response shape',
		);

		const invalidMediaEnum = resumeResponseWithAssets();
		const attachment = invalidMediaEnum.messages[0]!.attachments[0]!;
		(attachment.representations![0]!.availability as Record<string, unknown>).state = 'missing';
		expect(() => parseSessionResumeResponse(invalidMediaEnum)).toThrow(
			'Invalid session resume response shape',
		);

		const missingMediaField = resumeResponseWithAssets();
		delete missingMediaField.messages[0]!.media_inputs![0]!.asset.content_hash;
		expect(() => parseSessionResumeResponse(missingMediaField)).toThrow(
			'Invalid session resume response shape',
		);
	});

	it('preserves identity checks for retired fields, wrong owners, and unlinked rows', () => {
		const response = resumeResponse();
		const message = response.messages[0]!;
		message.id = message.message_id;
		delete (message as Record<string, unknown>).message_id;
		expect(() => parseSessionResumeResponse(response)).toThrow(
			'Invalid resumed message identity',
		);

		const badSession = resumeResponse();
		expect(() =>
			parseSessionResumeResponse(badSession, 'ses-00000000000000000000000000000007'),
		).toThrow('Invalid resumed session identity');

		const unlinkedMessage = resumeResponse();
		unlinkedMessage.steps = [];
		expect(() => parseSessionResumeResponse(unlinkedMessage)).toThrow(
			'Invalid resumed message identity',
		);

		const wrongOwner = resumeResponse();
		(wrongOwner.interactions[1]!.owner as Record<string, unknown>).session_id =
			'ses-00000000000000000000000000000006';
		expect(() => parseSessionResumeResponse(wrongOwner)).toThrow(
			'Invalid resumed interaction identity',
		);
	});

	it('validates shared msg step identities and nested managed asset identities', () => {
		const response = resumeResponseWithAssets();
		expect(parseSessionResumeResponse(response)).toBe(response);

		const malformed = resumeResponseWithAssets();
		malformed.messages[0]!.attachments[0]!.asset_id = 'asset-short';
		expect(() => parseSessionResumeResponse(malformed)).toThrow(
			'Invalid resumed message identity',
		);

		const malformedRepresentation = resumeResponseWithAssets();
		malformedRepresentation.messages[0]!.attachments[0]!.representations![0]!.payload.value.asset_id =
			'asset-short';
		expect(() => parseSessionResumeResponse(malformedRepresentation)).toThrow(
			'Invalid resumed message identity',
		);

		const malformedInputAsset = resumeResponseWithAssets();
		malformedInputAsset.messages[0]!.media_inputs![0]!.asset.asset_id = 'asset-short';
		expect(() => parseSessionResumeResponse(malformedInputAsset)).toThrow(
			'Invalid resumed message identity',
		);
	});

	it('matches Rust integer domains across steps, usage, interactions, and media', () => {
		const invalidStep = resumeResponse();
		invalidStep.steps[0]!.tool_index = 0x8000_0000;
		expect(() => parseSessionResumeResponse(invalidStep)).toThrow(
			'Invalid resumed numeric value',
		);

		const invalidTokenCount = resumeResponse();
		invalidTokenCount.usage = {
			...(invalidTokenCount.usage as Record<string, unknown>),
			prompt_tokens: 0x1_0000_0000,
		};
		expect(() => parseSessionResumeResponse(invalidTokenCount)).toThrow(
			'Invalid resumed numeric value',
		);

		const unsafeDuration = resumeResponse();
		unsafeDuration.llm_usage[0]!.duration_ms = Number.MAX_SAFE_INTEGER + 1;
		expect(() => parseSessionResumeResponse(unsafeDuration)).toThrow(
			'Invalid resumed numeric value',
		);

		const unsafeAssetSize = resumeResponseWithAssets();
		unsafeAssetSize.messages[0]!.media_inputs![0]!.asset.size_bytes =
			Number.MAX_SAFE_INTEGER + 1;
		expect(() => parseSessionResumeResponse(unsafeAssetSize)).toThrow(
			'Invalid resumed numeric value',
		);

		const tooManyEstimatedTokens = resumeResponseWithAssets();
		tooManyEstimatedTokens.messages[0]!.attachments[0]!.representations![0]!.cost!.estimated_tokens = 0x1_0000_0000;
		expect(() => parseSessionResumeResponse(tooManyEstimatedTokens)).toThrow(
			'Invalid resumed numeric value',
		);
	});
});
