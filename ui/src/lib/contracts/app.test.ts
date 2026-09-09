import { describe, expect, it } from 'vitest';
import { mapAppEvent } from './app.ts';

describe('app-shell IPC contract', () => {
	it('maps confirmation fields at the renderer boundary', () => {
		const event = mapAppEvent({
			event: 'confirm:requested',
			id: 1,
			payload: {
				step_id: 'conf-1',
				invocation_step_id: 'step-1',
				action_index: 1,
				tool_call_id: 'call-1',
				tool_name: 'run_command',
				risk_level: 'high',
				session_id: 'ses-1',
				summary: '将执行一条受保护的本机命令（命令内容不会显示在弹窗中）',
				permission_key: 'tool.run_command',
			},
		});

		expect(event.payload).toEqual({
			stepId: 'conf-1',
			invocationStepId: 'step-1',
			actionIndex: 1,
			toolCallId: 'call-1',
			toolName: 'run_command',
			riskLevel: 'high',
			sessionId: 'ses-1',
			summary: '将执行一条受保护的本机命令（命令内容不会显示在弹窗中）',
			permissionKey: 'tool.run_command',
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
