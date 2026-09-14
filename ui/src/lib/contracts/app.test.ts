import { describe, expect, it } from 'vitest';
import { mapAppEvent } from './app.ts';

describe('app-shell IPC contract', () => {
	it('maps interaction fields at the renderer boundary', () => {
		const event = mapAppEvent({
			event: 'interaction:requested',
			id: 1,
			payload: {
				id: 'conf-1',
				session_id: 'ses-1',
				kind: 'confirm',
				status: 'pending',
				prompt: 'Waiting for confirmation',
				options: [],
				invocation_step_id: 'step-1',
				action_index: 1,
				tool_call_id: 'call-1',
				tool_name: 'run_command',
				risk_level: 'high',
				created_at: '2026-01-01T00:00:00Z',
				summary: '将执行一条受保护的本机命令（命令内容不会显示在弹窗中）',
				permission_key: 'tool.run_command',
			},
		});

		expect(event.payload).toEqual({
			id: 'conf-1',
			sessionId: 'ses-1',
			kind: 'confirm',
			status: 'pending',
			prompt: 'Waiting for confirmation',
			options: [],
			invocationStepId: 'step-1',
			actionIndex: 1,
			toolCallId: 'call-1',
			toolName: 'run_command',
			riskLevel: 'high',
			summary: '将执行一条受保护的本机命令（命令内容不会显示在弹窗中）',
			permissionKey: 'tool.run_command',
			createdAt: '2026-01-01T00:00:00Z',
		});
		expect(event.payload).not.toHaveProperty('step_id');
	});

	it('keeps MCP status as a typed union', () => {
		const event = mapAppEvent({
			event: 'mcp:status_change',
			id: 2,
			payload: { name: 'filesystem', status: { Offline: { error: 'timeout' } } },
		});

		expect(event.payload).toEqual({
			name: 'filesystem',
			status: { Offline: { error: 'timeout' } },
		});
	});
});
