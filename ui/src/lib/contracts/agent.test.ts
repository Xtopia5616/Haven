import { describe, expect, it } from 'vitest';
import { mapAgentEvent } from './agent.ts';

describe('agent IPC contract', () => {
	it('maps execution identity fields to camelCase', () => {
		const event = mapAgentEvent({
			event: 'agent:action',
			id: 1,
			payload: {
				session_id: 'ses-1',
				tool_name: 'read_file',
				input: { path: 'C:/notes.txt' },
				step_number: 3,
				run_id: 2,
				tool_call_id: 'call-1',
				action_index: 0,
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
			actionIndex: 0,
			stepId: 'step-1',
			suppressStreamedThought: false,
			silent: true,
		});
		expect(event.payload).not.toHaveProperty('session_id');
	});

	it('preserves dynamic usage diagnostics while mapping fixed fields', () => {
		const event = mapAgentEvent({
			event: 'agent:usage',
			id: 2,
			payload: {
				session_id: 'ses-1',
				prompt_tokens: 10,
				completion_tokens: 4,
				total_tokens: 14,
				cached_tokens: 2,
				cache_creation_tokens: 1,
				cache_miss_tokens: 0,
				context_tokens: 20,
				cache_exclusive: false,
				cache_accounting: 'provider',
				cost_usd: null,
				model: 'model-a',
				cumulative_prompt_tokens: 10,
				cumulative_completion_tokens: 4,
				cumulative_total_tokens: 14,
				cumulative_cached_tokens: 2,
				cumulative_cache_creation_tokens: 1,
				cumulative_cache_miss_tokens: 0,
				cache_diagnostics: { source: 'provider' },
				cumulative_cost_usd: null,
				context_window: 128000,
				step_number: 3,
				duration_ms: 42,
				role: 'default_model',
				has_cost: false,
			},
		});

		expect(event.payload.sessionId).toBe('ses-1');
		expect(event.payload.promptTokens).toBe(10);
		expect(event.payload.cacheDiagnostics).toEqual({ source: 'provider' });
		expect(event.payload).not.toHaveProperty('prompt_tokens');
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

	it('maps outcome and event sequence for ordered tool observations', () => {
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
				action_index: 0,
				ask_options: [],
				step_id: 'step-1',
				outcome: 'unknown',
				idempotency: 'unknown',
				operation_scope: 'global',
				event_seq: 17,
			},
		});

		expect(event.payload.outcome).toBe('unknown');
		expect(event.payload.operationScope).toBe('global');
		expect(event.payload.eventSeq).toBe(17);
	});

	it('maps degraded compaction and keeps legacy payloads compatible', () => {
		const degraded = mapAgentEvent({
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
		const legacy = mapAgentEvent({
			event: 'agent:compaction',
			id: 5,
			payload: {
				session_id: 'ses-1',
				summary: 'prior turns',
				tokens_before: 1000,
				tokens_after: 400,
			},
		});

		expect(degraded.payload.degraded).toBe(true);
		expect(legacy.payload.degraded).toBe(false);
	});
});
