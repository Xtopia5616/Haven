import { describe, expect, it } from 'vitest';
import {
	mapAgentEvent as mapAgentEventContract,
	type AgentEventName,
	type AgentEventPayloadMap,
} from './agent.ts';
import type { TauriEvent } from './tauriEvent.ts';

function mapAgentEvent<K extends AgentEventName>(event: TauriEvent<unknown> & { event: K }) {
	const mapped = mapAgentEventContract(event);
	if (!mapped) throw new Error('Expected a valid agent event');
	return mapped as TauriEvent<AgentEventPayloadMap[K]>;
}

function makeUsagePayload(overrides: Record<string, unknown> = {}) {
	return {
		session_id: 'ses-1',
		prompt_tokens: 10,
		completion_tokens: 4,
		total_tokens: 14,
		cached_tokens: 2,
		cache_creation_tokens: 1,
		cache_miss_tokens: 0,
		context_tokens: 20,
		cache_exclusive: false,
		cache_accounting: 'inclusive',
		cost_usd: null,
		model: 'model-a',
		cumulative_prompt_tokens: 10,
		cumulative_completion_tokens: 4,
		cumulative_total_tokens: 14,
		cumulative_cached_tokens: 2,
		cumulative_cache_creation_tokens: 1,
		cumulative_cache_miss_tokens: 0,
		cache_diagnostics: {
			strategy: 'key',
			provider: 'openai',
			key_requested: true,
			system_split: false,
			downgraded: false,
			outcome: 'unknown',
			usage_source: 'unavailable',
		},
		cumulative_cost_usd: null,
		context_window: 128000,
		step_number: 3,
		duration_ms: 42,
		role: 'chat',
		call_kind: 'media',
		has_cost: false,
		...overrides,
	};
}

describe('agent IPC contract', () => {
	it('maps execution identity fields to camelCase', () => {
		const event = mapAgentEvent({
			event: 'agent:tool_call',
			id: 1,
			payload: {
				session_id: 'ses-1',
				tool_name: 'read_file',
				input: { path: 'C:/notes.txt' },
				step_number: 3,
				run_id: 2,
				tool_call_id: 'call-1',
				tool_index: 0,
				step_id: 'step-1',
				suppress_streamed_thought: false,
				silent: true,
			},
		});

		expect(event.payload).toEqual({
			sessionId: 'ses-1',
			toolName: 'read_file',
			input: { path: 'C:/notes.txt' },
			stepNumber: 3,
			runId: 2,
			toolCallId: 'call-1',
			toolIndex: 0,
			stepId: 'step-1',
			suppressStreamedThought: false,
			silent: true,
		});
		expect(event.payload).not.toHaveProperty('session_id');
	});

	it('maps typed usage kind and diagnostics', () => {
		const event = mapAgentEvent({
			event: 'agent:usage',
			id: 2,
			payload: makeUsagePayload(),
		});

		expect(event.payload.sessionId).toBe('ses-1');
		expect(event.payload.promptTokens).toBe(10);
		expect(event.payload.cacheDiagnostics).toEqual({
			strategy: 'key',
			provider: 'openai',
			key_requested: true,
			system_split: false,
			downgraded: false,
			outcome: 'unknown',
			usage_source: 'unavailable',
		});
		expect(event.payload.callKind).toBe('media');
		expect(event.payload).not.toHaveProperty('prompt_tokens');
	});

	it('rejects an unknown usage kind at the event boundary', () => {
		const mapped = mapAgentEventContract({
			event: 'agent:usage',
			id: 2,
			payload: makeUsagePayload({ call_kind: 'future_call_kind' }),
		});

		expect(mapped).toBeNull();
	});

	it('rejects unknown usage and cache diagnostic enums', () => {
		for (const payload of [
			makeUsagePayload({ role: 'future_request_kind' }),
			makeUsagePayload({ cache_accounting: 'provider' }),
			makeUsagePayload({
				cache_diagnostics: { ...makeUsagePayload().cache_diagnostics, strategy: 'future' },
			}),
		]) {
			expect(mapAgentEventContract({ event: 'agent:usage', id: 3, payload })).toBeNull();
		}
	});

	it('maps stream replacement boundaries to camelCase', () => {
		const event = mapAgentEvent({
			event: 'agent:stream_reset',
			id: 3,
			payload: {
				session_id: 'ses-1',
				step_number: 4,
				run_id: 8,
				thought_message_id: 'msg-thought',
				reasoning_message_id: 'msg-reasoning',
			},
		});

		expect(event.payload).toEqual({
			sessionId: 'ses-1',
			stepNumber: 4,
			runId: 8,
			thoughtMessageId: 'msg-thought',
			reasoningMessageId: 'msg-reasoning',
		});
	});

	it('maps media planning notices to the UI contract', () => {
		const event = mapAgentEvent({
			event: 'agent:media_plan',
			id: 7,
			payload: {
				session_id: 'ses-1',
				step_number: 4,
				run_id: 8,
				role: 'vision',
				strategy: 'auto',
				projections: [
					{
						asset_id: 'asset-1',
						representation: 'ocr_text',
						mode: 'derived',
					},
				],
				notices: [{ asset_id: 'asset-1', code: 'raw_capability_unknown' }],
			},
		});

		expect(event.payload).toEqual({
			sessionId: 'ses-1',
			stepNumber: 4,
			runId: 8,
			role: 'vision',
			strategy: 'auto',
			projections: [
				{
					assetId: 'asset-1',
					representation: 'ocr_text',
					mode: 'derived',
				},
			],
			notices: [{ assetId: 'asset-1', code: 'raw_capability_unknown' }],
		});
	});

	it('rejects unknown closed media-plan enum values but ignores additive fields', () => {
		const payload = {
			session_id: 'ses-1',
			step_number: 4,
			run_id: 8,
			role: 'vision',
			strategy: 'auto',
			projections: [
				{
					asset_id: 'asset-1',
					representation: 'ocr_text',
					mode: 'derived',
					provenance: 'added-wire-field',
				},
			],
			notices: [{ asset_id: 'asset-1', code: 'raw_capability_unknown', added: true }],
			event_seq: 9,
			added_wire_field: 'ignored',
		};
		const event = mapAgentEvent({ event: 'agent:media_plan', id: 8, payload });

		expect(event?.payload).toMatchObject({
			role: 'vision',
			strategy: 'auto',
			projections: [{ representation: 'ocr_text', mode: 'derived' }],
			notices: [{ code: 'raw_capability_unknown' }],
			eventSeq: 9,
		});
		expect(event?.payload).not.toHaveProperty('added_wire_field');

		const invalidPayloads = [
			{ ...payload, role: 'future_request_kind' },
			{ ...payload, strategy: 'future_strategy' },
			{
				...payload,
				projections: [
					{ ...payload.projections[0], representation: 'future_representation' },
				],
			},
			{ ...payload, projections: [{ ...payload.projections[0], mode: 'future_mode' }] },
			{ ...payload, notices: [{ ...payload.notices[0], code: 'future_notice' }] },
		];
		for (const invalidPayload of invalidPayloads) {
			expect(
				mapAgentEventContract({
					event: 'agent:media_plan',
					id: 9,
					payload: invalidPayload,
				}),
			).toBeNull();
		}
	});

	it('maps the canonical result envelope and event sequence for ordered tool observations', () => {
		const event = mapAgentEvent({
			event: 'agent:observation',
			id: 6,
			payload: {
				session_id: 'ses-1',
				observation: 'timeout',
				tool_name: 'messaging',
				step_number: 3,
				run_id: 2,
				silent: false,
				tool_call_id: 'call-1',
				tool_index: 0,
				ask_options: [],
				step_id: 'step-1',
				idempotency: 'unknown',
				operation_scope: 'global',
				renderer: 'messaging',
				result: {
					outcome: 'timed_out_unknown',
					error_class: 'unknown_outcome',
					retry_safety: 'non_idempotent',
					retryability: 'unknown',
					verification_hint: null,
					next_action: 'inspect',
					assets: ['asset-1'],
					added_wire_field: 'ignored',
				},
				event_seq: 17,
			},
		});

		expect(event.payload).not.toHaveProperty('outcome');
		expect(event.payload.operationScope).toBe('global');
		expect(event.payload.eventSeq).toBe(17);
		expect(event.payload.result).toEqual({
			outcome: 'timed_out_unknown',
			errorClass: 'unknown_outcome',
			retrySafety: 'non_idempotent',
			retryability: 'unknown',
			verificationHint: null,
			nextAction: 'inspect',
			assets: ['asset-1'],
		});
	});

	it('requires nullable tool_call_id and the observation result envelope', () => {
		const payload: Record<string, unknown> = {
			session_id: 'ses-1',
			observation: 'done',
			tool_name: 'files.read',
			step_number: 1,
			run_id: 1,
			silent: false,
			tool_call_id: null,
			tool_index: 0,
			ask_options: [],
			step_id: 'step-1',
			idempotency: 'idempotent',
			operation_scope: 'session',
			renderer: 'files',
			result: {
				outcome: 'succeeded',
				retry_safety: 'idempotent',
				retryability: 'not_retryable',
				assets: [],
			},
		};
		expect(
			mapAgentEventContract({ event: 'agent:observation', id: 1, payload }),
		).not.toBeNull();

		const missingCallId = { ...payload };
		delete missingCallId.tool_call_id;
		expect(
			mapAgentEventContract({ event: 'agent:observation', id: 1, payload: missingCallId }),
		).toBeNull();

		const missingResult = { ...payload };
		delete missingResult.result;
		expect(
			mapAgentEventContract({ event: 'agent:observation', id: 1, payload: missingResult }),
		).toBeNull();

		const toolRunPayload: Record<string, unknown> = {
			session_id: 'ses-1',
			tool_name: 'files.read',
			input: {},
			step_number: 1,
			run_id: 1,
			tool_index: 0,
			step_id: 'step-1',
			suppress_streamed_thought: false,
			silent: false,
		};
		expect(
			mapAgentEventContract({ event: 'agent:tool_call', id: 1, payload: toolRunPayload }),
		).toBeNull();
		toolRunPayload.tool_call_id = null;
		expect(
			mapAgentEventContract({ event: 'agent:tool_call', id: 1, payload: toolRunPayload }),
		).not.toBeNull();
	});

	it('accepts observations without a duplicate outcome and rejects unknown result enums', () => {
		const payload = {
			session_id: 'ses-1',
			observation: 'result',
			tool_name: 'files.read',
			step_number: 1,
			run_id: 1,
			silent: false,
			tool_call_id: null,
			tool_index: 0,
			ask_options: [],
			step_id: 'step-1',
			idempotency: 'unknown',
			operation_scope: 'session',
			renderer: 'files',
			result: {
				outcome: 'succeeded',
				retry_safety: 'idempotent',
				retryability: 'retryable',
				assets: [],
			},
		};

		const withoutRenderer: Record<string, unknown> = { ...payload };
		delete withoutRenderer.renderer;
		expect(
			mapAgentEventContract({ event: 'agent:observation', id: 8, payload: withoutRenderer }),
		).toBeNull();
		expect(
			mapAgentEventContract({ event: 'agent:observation', id: 8, payload }),
		).not.toBeNull();
		const unknownRetrySafety = {
			...payload,
			result: { ...payload.result, retry_safety: 'future_retry_safety' },
		};
		expect(
			mapAgentEventContract({
				event: 'agent:observation',
				id: 8,
				payload: unknownRetrySafety,
			}),
		).toBeNull();
		for (const [field, value] of [
			['idempotency', 'future_idempotency'],
			['operation_scope', 'future_scope'],
		] as const) {
			expect(
				mapAgentEventContract({
					event: 'agent:observation',
					id: 8,
					payload: { ...payload, [field]: value },
				}),
			).toBeNull();
		}
		payload.result.outcome = 'future_outcome';
		expect(mapAgentEventContract({ event: 'agent:observation', id: 8, payload })).toBeNull();
	});

	it('maps the current compaction payload', () => {
		const event = mapAgentEvent({
			event: 'agent:compaction',
			id: 4,
			payload: {
				session_id: 'ses-1',
				summary: '[older context omitted]',
				tokens_before: 1000,
				tokens_after: 400,
				degraded: true,
			},
		});

		expect(event.payload.degraded).toBe(true);
	});

	it('maps the committed thought sequence', () => {
		const event = mapAgentEvent({
			event: 'agent:thought',
			id: 7,
			payload: {
				session_id: 'ses-1',
				thought: 'keep',
				step_number: 2,
				run_id: 4,
				message_id: 'step-keep',
				event_seq: 9,
			},
		});

		expect(event.payload.messageId).toBe('step-keep');
		expect(event.payload.eventSeq).toBe(9);
	});

	it('rejects malformed agent envelopes and required fields without throwing', () => {
		expect(
			mapAgentEventContract({
				event: 'agent:thought',
				id: 1,
				payload: {
					session_id: 'ses-1',
					thought: 17,
					step_number: 2,
					run_id: 4,
					message_id: 'step-1',
				},
			}),
		).toBeNull();
		expect(
			mapAgentEventContract({
				event: 'agent:media_plan',
				id: 1,
				payload: {
					session_id: 'ses-1',
					step_number: 2,
					run_id: 4,
					role: 'vision',
					strategy: 'auto',
					projections: [{ asset_id: 'asset-1' }],
					notices: [],
				},
			}),
		).toBeNull();
		expect(mapAgentEventContract({ event: 'agent:future', id: 1, payload: {} })).toBeNull();
		expect(
			mapAgentEventContract({ event: 'agent:thought', id: Number.NaN, payload: {} }),
		).toBeNull();
	});

	it('preserves empty notification text for the existing UI fallback', () => {
		const event = mapAgentEvent({
			event: 'notification:show',
			id: 11,
			payload: { session_id: 'ses-1', title: '', body: '' },
		});

		expect(event.payload).toEqual({ sessionId: 'ses-1', title: '', body: '' });
	});

	it('maps app notifications without inventing a session association', () => {
		const event = mapAgentEvent({
			event: 'notification:show',
			id: 15,
			payload: { title: '操作已完成', body: '结果' },
		});

		expect(event.payload).toEqual({ title: '操作已完成', body: '结果' });
	});

	it('maps marked ToolRun completions without relaxing generic session validation', () => {
		const scheduled = mapAgentEvent({
			event: 'notification:show',
			id: 12,
			payload: {
				title: '任务完成',
				body: '结果',
				notification_kind: 'tool_run_completion',
				tool_run_kind: 'scheduled',
				tool_run_id: 'toolrun-scheduled',
			},
		});
		expect(scheduled.payload).toEqual({
			title: '任务完成',
			body: '结果',
			notificationKind: 'tool_run_completion',
			toolRunKind: 'scheduled',
			toolRunId: 'toolrun-scheduled',
		});

		const background = mapAgentEvent({
			event: 'notification:show',
			id: 13,
			payload: {
				session_id: 'ses-background',
				title: '后台任务失败',
				body: '执行失败',
				notification_kind: 'tool_run_completion',
				tool_run_kind: 'background',
				tool_run_id: 'toolrun-background',
				tool_run_status: 'failed',
			},
		});
		expect(background.payload).toMatchObject({
			sessionId: 'ses-background',
			toolRunKind: 'background',
			toolRunId: 'toolrun-background',
			toolRunStatus: 'failed',
		});
		const completed = mapAgentEvent({
			event: 'notification:show',
			id: 15,
			payload: {
				session_id: 'ses-background',
				title: '后台任务已完成',
				body: '完成',
				notification_kind: 'tool_run_completion',
				tool_run_kind: 'background',
				tool_run_id: 'toolrun-completed',
				tool_run_status: 'completed',
			},
		});
		expect(completed.payload).toMatchObject({ toolRunStatus: 'completed' });

		for (const payload of [
			{ session_id: '', title: 'generic', body: 'body' },
			{
				session_id: 'ses-1',
				title: 'x',
				body: 'y',
				notification_kind: 'unknown',
			},
			{
				session_id: '',
				title: 'x',
				body: 'y',
				notification_kind: 'tool_run_completion',
				tool_run_kind: 'background',
				tool_run_id: 'toolrun-1',
				tool_run_status: 'failed',
			},
			{
				session_id: 'ses-1',
				title: 'x',
				body: 'y',
				notification_kind: 'tool_run_completion',
				tool_run_kind: 'scheduled',
				tool_run_id: 'toolrun-1',
				tool_run_status: 'failed',
			},
			{
				session_id: 'ses-1',
				title: 'x',
				body: 'y',
				notification_kind: 'tool_run_completion',
				tool_run_kind: 'foreground',
				tool_run_id: 'toolrun-1',
				tool_run_status: 'failed',
			},
			{
				session_id: 'ses-1',
				title: 'x',
				body: 'y',
				notification_kind: 'tool_run_completion',
				tool_run_kind: 'background',
				tool_run_id: 'toolrun-1',
				tool_run_status: 'cancelled',
			},
		]) {
			expect(
				mapAgentEventContract({ event: 'notification:show', id: 14, payload }),
			).toBeNull();
		}
	});

	it.each([
		{
			event: 'agent:thought_chunk',
			wire: {
				session_id: 'ses-1',
				delta: 't',
				step_number: 1,
				run_id: 2,
				message_id: 'msg-1',
				seq: 3,
			},
			expected: {
				sessionId: 'ses-1',
				delta: 't',
				stepNumber: 1,
				runId: 2,
				messageId: 'msg-1',
				seq: 3,
			},
		},
		{
			event: 'agent:reasoning_chunk',
			wire: {
				session_id: 'ses-1',
				delta: 'r',
				step_number: 1,
				run_id: 2,
				message_id: 'msg-2',
				seq: 4,
			},
			expected: {
				sessionId: 'ses-1',
				delta: 'r',
				stepNumber: 1,
				runId: 2,
				messageId: 'msg-2',
				seq: 4,
			},
		},
		{
			event: 'agent:web_search',
			wire: {
				session_id: 'ses-1',
				phase: 'started',
				step_number: 2,
				run_id: 3,
				call_id: 'call-1',
				action: 'search',
				result: { count: 1 },
			},
			expected: {
				sessionId: 'ses-1',
				phase: 'started',
				stepNumber: 2,
				runId: 3,
				callId: 'call-1',
				action: 'search',
				result: { count: 1 },
			},
		},
		{
			event: 'agent:stream_stalled',
			wire: { session_id: 'ses-1' },
			expected: { sessionId: 'ses-1' },
		},
		{
			event: 'agent:supplement',
			wire: {
				session_id: 'ses-1',
				additional_context: 'continue',
				step_number: 2,
				run_id: 3,
				message_id: 'msg-1',
				supplement_id: 'msg-1',
				inject_source: 'future_source',
				event_seq: 9,
			},
			expected: {
				sessionId: 'ses-1',
				additionalContext: 'continue',
				stepNumber: 2,
				runId: 3,
				messageId: 'msg-1',
				supplementId: 'msg-1',
				injectSource: 'future_source',
				eventSeq: 9,
			},
		},
		{
			event: 'agent:tool_output',
			wire: { session_id: 'ses-1', step_id: 'step-1', output: 'partial output' },
			expected: { sessionId: 'ses-1', stepId: 'step-1', output: 'partial output' },
		},
	] as const)('maps $event payloads at the agent boundary', ({ event, wire, expected }) => {
		const mapped = mapAgentEventContract({ event, id: 12, payload: wire });
		expect(mapped?.payload).toEqual(expected);
	});
});
