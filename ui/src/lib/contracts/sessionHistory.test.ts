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

function resumeResponse() {
	return {
		session: { id: sessionId },
		messages: [
			{ id: messageId, session_id: sessionId, tool_call_id: 'provider-call/opaque' },
			{ id: stepId, session_id: sessionId, tool_call_id: 'another-external-id' },
		],
		steps: [{ id: stepId, session_id: sessionId }],
		llm_usage: [
			{
				id: 'usage-00000000000000000000000000000004',
				session_id: sessionId,
			},
		],
		interactions: [
			{
				id: stepId,
				kind: 'ask',
				session_id: sessionId,
				owner: { kind: 'session', session_id: sessionId },
			},
			{
				id: 'conf-00000000000000000000000000000005',
				kind: 'confirm',
				session_id: sessionId,
				owner: { kind: 'session', session_id: sessionId },
			},
		],
	};
}

describe('session history and resume identity contracts', () => {
	it('validates runtime session and step IDs', () => {
		expect(
			parseRuntimeSessionListResponse({
				sessions: [{ id: sessionId, steps: [{ id: stepId }] }],
			}).sessions,
		).toHaveLength(1);
		expect(() =>
			parseRuntimeSessionListResponse({
				sessions: [{ id: sessionId, steps: [{ id: 'step-short' }] }],
			}),
		).toThrow('Invalid runtime session identity');
	});

	it('validates persisted session IDs for history and lineage', () => {
		expect(parseSessionHistoryRows([{ id: sessionId }])).toHaveLength(1);
		expect(() =>
			parseSessionHistoryRows([{ id: 'msg-00000000000000000000000000000002' }]),
		).toThrow('Invalid session history identity');
		expect(
			parseSessionLineageResponse({ parent: null, children: [{ id: sessionId }] }).children,
		).toHaveLength(1);
	});

	it('accepts resume IDs with canonical row linkage and opaque provider IDs', () => {
		const response = resumeResponse();
		expect(parseSessionResumeResponse(response)).toBe(response);
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
		badUsage.llm_usage[0].id = 'usage-short';
		expect(() => parseSessionResumeResponse(badUsage)).toThrow(
			'Invalid resumed usage identity',
		);

		const badInteraction = resumeResponse();
		badInteraction.interactions[1].id = 'step-00000000000000000000000000000005';
		expect(() => parseSessionResumeResponse(badInteraction)).toThrow(
			'Invalid resumed interaction identity',
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
