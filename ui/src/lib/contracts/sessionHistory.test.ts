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

type ResumeMediaRepresentationFixture = {
	payload: { kind: string; value?: Record<string, unknown> };
};
type ResumeMessageFixture = {
	message_id: string;
	session_id: string;
	tool_call_id?: string;
	attachments: Array<{
		asset_id?: string;
		representations?: ResumeMediaRepresentationFixture[];
	}>;
	media_inputs?: Array<{
		asset: { asset_id: string };
		representations: ResumeMediaRepresentationFixture[];
	}>;
};
type ResumeFixture = {
	session: { session_id: string };
	messages: ResumeMessageFixture[];
	steps: Array<{ step_id: string; session_id: string }>;
	llm_usage: Array<{ usage_id: string; session_id: string }>;
	interactions: Array<{
		request_id: string;
		kind: 'ask' | 'confirm';
		session_id: string;
		owner: { kind: 'session'; session_id: string };
	}>;
};

function resumeResponse(): ResumeFixture {
	return {
		session: { session_id: sessionId },
		messages: [
			{
				message_id: messageId,
				session_id: sessionId,
				tool_call_id: 'provider-call/opaque',
				attachments: [],
			},
			{
				message_id: stepId,
				session_id: sessionId,
				tool_call_id: 'another-external-id',
				attachments: [],
			},
		],
		steps: [{ step_id: stepId, session_id: sessionId }],
		llm_usage: [
			{
				usage_id: 'usage-00000000000000000000000000000004',
				session_id: sessionId,
			},
		],
		interactions: [
			{
				request_id: stepId,
				kind: 'ask',
				session_id: sessionId,
				owner: { kind: 'session', session_id: sessionId },
			},
			{
				request_id: 'conf-00000000000000000000000000000005',
				kind: 'confirm',
				session_id: sessionId,
				owner: { kind: 'session', session_id: sessionId },
			},
		],
	};
}

function resumeResponseWithAssets(): ResumeFixture {
	const response = resumeResponse();
	response.steps.push({ step_id: messageId, session_id: sessionId });
	response.messages = [
		{
			message_id: messageId,
			session_id: sessionId,
			tool_call_id: 'provider-call/opaque',
			attachments: [
				{
					asset_id: 'asset-00000000000000000000000000000009',
					representations: [
						{
							payload: {
								kind: 'managed_file_ref',
								value: { asset_id: 'asset-00000000000000000000000000000010' },
							},
						},
					],
				},
			],
			media_inputs: [
				{
					asset: { asset_id: 'asset-00000000000000000000000000000011' },
					representations: [
						{
							payload: {
								kind: 'managed_file_ref',
								value: { asset_id: 'asset-00000000000000000000000000000012' },
							},
						},
					],
				},
			],
		},
	];
	return response;
}

describe('session history and resume identity contracts', () => {
	it('validates runtime session and step IDs', () => {
		expect(
			parseRuntimeSessionListResponse({
				sessions: [{ session_id: sessionId, steps: [{ step_id: stepId }] }],
			}).sessions,
		).toHaveLength(1);
		expect(() =>
			parseRuntimeSessionListResponse({
				sessions: [{ session_id: sessionId, steps: [{ step_id: 'step-short' }] }],
			}),
		).toThrow('Invalid runtime session identity');
		expect(() =>
			parseRuntimeSessionListResponse({
				sessions: [{ id: sessionId, steps: [{ id: stepId }] }],
			}),
		).toThrow('Invalid runtime session identity');
	});

	it('validates persisted session IDs for history and lineage', () => {
		expect(parseSessionHistoryRows([{ session_id: sessionId }])).toHaveLength(1);
		expect(() => parseSessionHistoryRows([{ id: sessionId }])).toThrow(
			'Invalid session history identity',
		);
		expect(() =>
			parseSessionHistoryRows([{ session_id: 'msg-00000000000000000000000000000002' }]),
		).toThrow('Invalid session history identity');
		expect(
			parseSessionLineageResponse(
				{
					session_id: sessionId,
					parent_session_id: null,
					parent: null,
					children: [
						{
							session: { session_id: 'ses-00000000000000000000000000000006' },
							parent_session_id: sessionId,
						},
					],
				},
				sessionId,
			).children,
		).toHaveLength(1);
	});

	it('accepts resume IDs with canonical row linkage and opaque provider IDs', () => {
		const response = resumeResponse();
		expect(parseSessionResumeResponse(response)).toBe(response);
	});

	it('rejects the retired generic message identity field', () => {
		const response = resumeResponse();
		const message = response.messages[0] as unknown as Record<string, unknown>;
		message.id = message.message_id;
		delete message.message_id;

		expect(() => parseSessionResumeResponse(response)).toThrow(
			'Invalid resumed message identity',
		);
	});

	it('requires resume step rows to use step_id', () => {
		const response = resumeResponse();
		response.steps = [{ id: stepId, session_id: sessionId } as never];
		expect(() => parseSessionResumeResponse(response)).toThrow(
			'Invalid resumed step identity',
		);
	});

	it('binds resume and lineage rows to the requested owner', () => {
		const response = resumeResponse();
		expect(() =>
			parseSessionResumeResponse(response, 'ses-00000000000000000000000000000007'),
		).toThrow('Invalid resumed session identity');

		expect(() =>
			parseSessionLineageResponse(
				{
					session_id: 'ses-00000000000000000000000000000008',
					parent_session_id: null,
					parent: null,
					children: [],
				},
				sessionId,
			),
		).toThrow('Invalid session lineage identity');
	});

	it('checks the parent and direct-child relationship anchors in lineage responses', () => {
		expect(() =>
			parseSessionLineageResponse(
				{
					session_id: sessionId,
					parent_session_id: 'ses-00000000000000000000000000000040',
				parent: { session_id: 'ses-00000000000000000000000000000041' },
					children: [],
				},
				sessionId,
			),
		).toThrow('Invalid session lineage identity');

		expect(() =>
			parseSessionLineageResponse(
				{
					session_id: sessionId,
					parent_session_id: null,
					parent: null,
					children: [
						{
							session: { session_id: 'ses-00000000000000000000000000000042' },
							parent_session_id: 'ses-00000000000000000000000000000043',
						},
					],
				},
				sessionId,
			),
		).toThrow('Invalid session lineage identity');
	});

	it('allows supplement steps to reuse msg IDs and checks nested managed asset IDs', () => {
		const response = resumeResponseWithAssets();
		expect(parseSessionResumeResponse(response)).toBe(response);

		const malformedAssetResponses = [
			() => {
				const bad = resumeResponseWithAssets();
				bad.messages[0].attachments[0].asset_id = 'asset-short';
				return bad;
			},
			() => {
				const bad = resumeResponseWithAssets();
				bad.messages[0].attachments[0].representations![0].payload.value!.asset_id =
					'asset-short';
				return bad;
			},
			() => {
				const bad = resumeResponseWithAssets();
				bad.messages[0].media_inputs![0].asset.asset_id = 'asset-short';
				return bad;
			},
			() => {
				const bad = resumeResponseWithAssets();
				bad.messages[0].media_inputs![0].representations[0].payload.value!.asset_id =
					'asset-short';
				return bad;
			},
		];
		for (const malformed of malformedAssetResponses) {
			expect(() => parseSessionResumeResponse(malformed())).toThrow(
				'Invalid resumed message identity',
			);
		}
	});

	it('rejects a message that refers to a step absent from the resumed step rows', () => {
		const response = resumeResponse();
		response.steps = [];
		expect(() => parseSessionResumeResponse(response)).toThrow(
			'Invalid resumed message identity',
		);
	});

	it('rejects non-canonical usage and interaction identities', () => {
		const badUsage = resumeResponse();
		badUsage.llm_usage[0].usage_id = 'usage-short';
		expect(() => parseSessionResumeResponse(badUsage)).toThrow(
			'Invalid resumed usage identity',
		);

		const badInteraction = resumeResponse();
		badInteraction.interactions[1].request_id = 'step-00000000000000000000000000000005';
		expect(() => parseSessionResumeResponse(badInteraction)).toThrow(
			'Invalid resumed interaction identity',
		);
	});

	it('rejects the retired generic usage identity field', () => {
		const response = resumeResponse();
		const usage = response.llm_usage[0] as Record<string, unknown>;
		usage.id = usage.usage_id;
		delete usage.usage_id;

		expect(() => parseSessionResumeResponse(response)).toThrow(
			'Invalid resumed usage identity',
		);
	});

	it('rejects interactions owned by a different session', () => {
		const response = resumeResponse();
		response.interactions[1].owner.session_id = 'ses-00000000000000000000000000000006';
		expect(() => parseSessionResumeResponse(response)).toThrow(
			'Invalid resumed interaction identity',
		);
	});
});
