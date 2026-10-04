import { describe, expect, it } from 'vitest';
import { interactionOwnerToWire, mapAppEvent } from './app.ts';

describe('app-shell IPC contract', () => {
	it.each([
		[
			{ kind: 'session', sessionId: 'ses-1' } as const,
			{ kind: 'session', session_id: 'ses-1' },
		],
		[
			{ kind: 'scheduled_action', actionId: 'act-1' } as const,
			{ kind: 'scheduled_action', action_id: 'act-1' },
		],
		[{ kind: 'app_command' } as const, { kind: 'app_command' }],
	])('serializes the selected interaction owner for resolve IPC', (owner, wire) => {
		expect(interactionOwnerToWire(owner)).toEqual(wire);
	});

	it('maps interaction fields at the renderer boundary', () => {
		const event = mapAppEvent({
			event: 'interaction:requested',
			id: 1,
			payload: {
				id: 'conf-1',
				session_id: 'ses-1',
				owner: { kind: 'session', session_id: 'ses-1' },
				kind: 'confirm',
				status: 'pending',
				options: [],
				invocation_step_id: 'step-1',
				action_index: 1,
				tool_call_id: 'call-1',
				tool_name: 'run_command',
				risk_level: 'high',
				created_at: '2026-01-01T00:00:00Z',
				summary: '将执行一条受保护的本机命令（命令内容不会显示在弹窗中）',
				permission_key: 'tool.run_command',
				future_field: 'not part of the renderer DTO',
			},
		});

		expect(event?.payload).toEqual({
			id: 'conf-1',
			sessionId: 'ses-1',
			owner: { kind: 'session', sessionId: 'ses-1' },
			kind: 'confirm',
			status: 'pending',
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
		expect(event?.payload).not.toHaveProperty('step_id');
		expect(event?.payload).not.toHaveProperty('future_field');
	});

	it('rejects unknown interaction enum values while defaulting omitted options', () => {
		const event = mapAppEvent({
			event: 'interaction:requested',
			id: 2,
			payload: {
				id: 'interaction-1',
				session_id: 'ses-1',
				owner: { kind: 'session', session_id: 'ses-1' },
				kind: 'future_kind',
				status: 'future_status',
				created_at: '2026-01-01T00:00:00Z',
				risk_level: 'future_risk',
			},
		});

		expect(event).toBeNull();
	});

	it.each([
		{
			owner: { kind: 'app_command' },
			expectedOwner: { kind: 'app_command' },
		},
		{
			owner: { kind: 'scheduled_action', action_id: 'act-1' },
		expectedOwner: { kind: 'scheduled_action', actionId: 'act-1' },
		},
		{
			owner: { kind: 'scheduled_action', action_id: 'act-1' },
			session_id: 'ses-context',
		expectedOwner: { kind: 'scheduled_action', actionId: 'act-1' },
		},
	])('maps explicit owner routes and optional session context', ({ owner, session_id, expectedOwner }) => {
		const event = mapAppEvent({
			event: 'interaction:requested',
			id: 20,
			payload: {
				id: 'conf-owner',
				...(session_id ? { session_id } : {}),
				owner,
				kind: 'confirm',
				status: 'pending',
				created_at: '2026-01-01T00:00:00Z',
			},
		});

		expect(event?.payload).toMatchObject({
			id: 'conf-owner',
			owner: expectedOwner,
		});
		if (session_id) expect(event?.payload).toMatchObject({ sessionId: session_id });
		else expect(event?.payload).not.toHaveProperty('sessionId');
	});

	it.each([
		{ owner: undefined, session_id: 'ses-1' },
		{ owner: { kind: 'session', session_id: 'ses-other' }, session_id: 'ses-1' },
		{ owner: { kind: 'session' }, session_id: 'ses-1' },
		{ owner: { kind: 'scheduled_action' }, session_id: undefined },
		{ owner: { kind: 'app_command' }, session_id: 'ses-1' },
		{ owner: { kind: 'future_owner', action_id: 'act-1' }, session_id: undefined },
	])('rejects an invalid owner/context pair', ({ owner, session_id }) => {
		const event = mapAppEvent({
			event: 'interaction:requested',
			id: 21,
			payload: {
				id: 'conf-invalid-owner',
				...(session_id ? { session_id } : {}),
				owner,
				kind: 'confirm',
				status: 'pending',
				created_at: '2026-01-01T00:00:00Z',
			},
		});
		expect(event).toBeNull();
	});

	it('rejects MCP status variants outside the current enum', () => {
		const payload = {
			name: 'filesystem',
			status: { FutureStatus: { retry_after_ms: 500, detail: 'future' } },
			future_field: 'preserved',
		};
		const event = mapAppEvent({ event: 'mcp:status_change', id: 3, payload });

		expect(event).toBeNull();
	});

	it('projects only the current MCP status DTO fields', () => {
		const payload = {
			name: 'filesystem',
			status: { Offline: { error: 'timeout' } },
			future_field: 'preserved',
		};
		const event = mapAppEvent({ event: 'mcp:status_change', id: 4, payload });

		expect(event?.payload).toEqual({
			name: 'filesystem',
			status: { Offline: { error: 'timeout' } },
		});
	});

	it('preserves confirmed MCP refresh failures on the existing status channel', () => {
		const payload = {
			name: 'new-server',
			status: { Offline: { error: 'MCP 连接失败，请检查服务器状态或配置' } },
		};
		const event = mapAppEvent({ event: 'mcp:status_change', id: 5, payload });

		expect(event?.payload).toEqual(payload);
	});

	it.each(['refresh', 'auto_refresh', 'toggle'] as const)(
		'validates and preserves the Skills status wrapper for %s',
		(op) => {
			const payload = { op, future_field: 'preserved' };
			const event = mapAppEvent({ event: 'skills:status_change', id: 5, payload });

			expect(event?.payload).toEqual(payload);
		},
	);

	it('maps hotkey rebind fields once and ignores wire extensions', () => {
		const event = mapAppEvent({
			event: 'hotkey:rebind',
			id: 6,
			payload: {
				old_binding: 'Ctrl+Shift+Space',
				new_binding: 'Ctrl+Alt+Space',
				future_field: 'ignored',
			},
		});

		expect(event?.payload).toEqual({
			oldBinding: 'Ctrl+Shift+Space',
			newBinding: 'Ctrl+Alt+Space',
		});
	});

	it.each([
		{ event: 'hotkey:rebind', payload: { old_binding: 1, new_binding: 'Ctrl+B' } },
		{ event: 'hotkey:conflict', payload: { binding: 'Ctrl+A', error: null } },
		{
			event: 'interaction:requested',
			payload: {
				id: 'conf-1',
				session_id: 'ses-1',
				owner: { kind: 'session', session_id: 'ses-1' },
				kind: 'confirm',
				status: 'pending',
				created_at: 'now',
				options: ['okay', 1],
			},
		},
		{
			event: 'interaction:requested',
			payload: {
				id: 'conf-1',
				session_id: 'ses-1',
				owner: { kind: 'session', session_id: 'ses-1' },
				kind: 'confirm',
				status: 'pending',
				created_at: 'now',
				summary: null,
			},
		},
		{
			event: 'interaction:requested',
			payload: {
				id: 'conf-1',
				session_id: 'ses-1',
				owner: { kind: 'session', session_id: 'ses-1' },
				kind: 'confirm',
				status: 'pending',
				created_at: 'now',
				action_index: null,
			},
		},
		{
			event: 'interaction:requested',
			payload: {
				id: 'conf-1',
				session_id: 'ses-1',
				owner: { kind: 'session', session_id: 'ses-1' },
				kind: 'confirm',
				status: 'pending',
				created_at: 'now',
				action_index: -1,
			},
		},
		{ event: 'mcp:status_change', payload: { name: 'server', status: 42 } },
		{ event: 'mcp:status_change', payload: { name: 'server', status: [] } },
		{ event: 'skills:status_change', payload: { op: false } },
		{ event: 'skills:status_change', payload: { op: 'future_op' } },
		{ event: 'app:bootstrap', payload: { status: 'complete' } },
		{ event: 'tray:status_changed', payload: { status: 'idle', tooltip: 'Haven' } },
	])('drops malformed required app payloads: $event', ({ event, payload }) => {
		expect(mapAppEvent({ event, id: 7, payload })).toBeNull();
	});

	it('drops malformed Tauri app event envelopes', () => {
		expect(mapAppEvent({ event: 'hotkey:rebind', id: Number.NaN, payload: {} })).toBeNull();
		expect(mapAppEvent({ event: 'unknown:event', id: 1, payload: {} })).toBeNull();
		expect(mapAppEvent({ event: 'hotkey:rebind', id: 1, payload: null })).toBeNull();
	});
});
