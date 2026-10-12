import { describe, expect, it } from 'vitest';
import { interactionOwnerToWire, mapAppEvent, parseConfirmationResolutionResult } from './app.ts';

describe('app-shell IPC contract', () => {
	it.each(['resolved', 'expired', 'stale'])('accepts confirmation result %s', (result) => {
		expect(parseConfirmationResolutionResult(result)).toBe(result);
	});

	it.each(['pending', 'unknown', null, {}])(
		'rejects invalid confirmation result %j',
		(result) => {
			expect(() => parseConfirmationResolutionResult(result)).toThrow(
				'Invalid confirmation resolution result',
			);
		},
	);

	it.each([
		[
			{ kind: 'session', sessionId: 'ses-6b86b273ff34fce19d6b804eff5a3f57' } as const,
			{ kind: 'session', session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57' },
		],
		[
			{
				kind: 'scheduled_tool_run',
				toolRunId: 'toolrun-6b86b273ff34fce19d6b804eff5a3f57',
			} as const,
			{ kind: 'scheduled_tool_run', tool_run_id: 'toolrun-6b86b273ff34fce19d6b804eff5a3f57' },
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
				request_id: 'conf-6b86b273ff34fce19d6b804eff5a3f57',
				session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57',
				owner: { kind: 'session', session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57' },
				kind: 'confirm',
				status: 'pending',
				options: [],
				invocation_step_id: 'step-6b86b273ff34fce19d6b804eff5a3f57',
				tool_index: 1,
				tool_call_id: 'call-1',
				tool_name: 'run_command',
				risk_level: 'high',
				created_at: '2026-01-01T00:00:00Z',
				expires_at: '2026-01-01T00:05:00Z',
				summary: '将执行一条受保护的本机命令（命令内容不会显示在弹窗中）',
				permission_key: 'tool.run_command',
				future_field: 'not part of the renderer DTO',
			},
		});

		expect(event?.payload).toEqual({
			requestId: 'conf-6b86b273ff34fce19d6b804eff5a3f57',
			sessionId: 'ses-6b86b273ff34fce19d6b804eff5a3f57',
			owner: { kind: 'session', sessionId: 'ses-6b86b273ff34fce19d6b804eff5a3f57' },
			kind: 'confirm',
			status: 'pending',
			options: [],
			invocationStepId: 'step-6b86b273ff34fce19d6b804eff5a3f57',
			toolIndex: 1,
			toolCallId: 'call-1',
			toolName: 'run_command',
			riskLevel: 'high',
			summary: '将执行一条受保护的本机命令（命令内容不会显示在弹窗中）',
			permissionKey: 'tool.run_command',
			createdAt: '2026-01-01T00:00:00Z',
			expiresAt: '2026-01-01T00:05:00Z',
		});
		expect(event?.payload).not.toHaveProperty('step_id');
		expect(event?.payload).not.toHaveProperty('future_field');
	});

	it('accepts a Session-owned Ask interaction', () => {
		const event = mapAppEvent({
			event: 'interaction:requested',
			id: 24,
			payload: {
				request_id: 'step-e7f729190d36bf14aed6c9c2ed7f5fe7',
				session_id: 'ses-e7f729190d36bf14aed6c9c2ed7f5fe7',
				owner: { kind: 'session', session_id: 'ses-e7f729190d36bf14aed6c9c2ed7f5fe7' },
				kind: 'ask',
				status: 'pending',
				options: ['继续', '停止'],
				created_at: '2026-01-01T00:00:00Z',
			},
		});

		expect(event?.payload).toMatchObject({
			requestId: 'step-e7f729190d36bf14aed6c9c2ed7f5fe7',
			kind: 'ask',
			owner: { kind: 'session', sessionId: 'ses-e7f729190d36bf14aed6c9c2ed7f5fe7' },
		});
	});

	it('rejects the retired generic interaction id field', () => {
		const event = mapAppEvent({
			event: 'interaction:requested',
			id: 25,
			payload: {
				id: 'conf-6b86b273ff34fce19d6b804eff5a3f57',
				session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57',
				owner: { kind: 'session', session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57' },
				kind: 'confirm',
				status: 'pending',
				options: [],
				created_at: '2026-01-01T00:00:00Z',
				expires_at: '2026-01-01T00:05:00Z',
			},
		});

		expect(event).toBeNull();
	});

	it('rejects interaction identities that do not match their entity type', () => {
		const confirmation = {
			request_id: 'conf-0123456789abcdef0123456789abcdef',
			session_id: 'ses-0123456789abcdef0123456789abcdef',
			owner: { kind: 'session', session_id: 'ses-0123456789abcdef0123456789abcdef' },
			kind: 'confirm',
			status: 'pending',
			created_at: '2026-01-01T00:00:00Z',
			expires_at: '2026-01-01T00:05:00Z',
		};
		for (const payload of [
			{ ...confirmation, request_id: 'step-0123456789abcdef0123456789abcdef' },
			{ ...confirmation, session_id: 'msg-0123456789abcdef0123456789abcdef' },
			{ ...confirmation, invocation_step_id: 'msg-0123456789abcdef0123456789abcdef' },
			{
				...confirmation,
				owner: { kind: 'session', session_id: 'toolrun-0123456789abcdef0123456789abcdef' },
			},
			{
				...confirmation,
				request_id: 'conf-short',
			},
			{
				...confirmation,
				kind: 'ask',
			},
		]) {
			expect(mapAppEvent({ event: 'interaction:requested', id: 30, payload })).toBeNull();
		}
	});

	it('rejects unknown interaction enum values while defaulting omitted options', () => {
		const event = mapAppEvent({
			event: 'interaction:requested',
			id: 2,
			payload: {
				request_id: 'interaction-1',
				session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57',
				owner: { kind: 'session', session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57' },
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
			kind: 'confirm',
		},
		{
			owner: {
				kind: 'scheduled_tool_run',
				tool_run_id: 'toolrun-6b86b273ff34fce19d6b804eff5a3f57',
			},
			expectedOwner: {
				kind: 'scheduled_tool_run',
				toolRunId: 'toolrun-6b86b273ff34fce19d6b804eff5a3f57',
			},
			kind: 'scheduled_confirm',
		},
		{
			owner: {
				kind: 'scheduled_tool_run',
				tool_run_id: 'toolrun-6b86b273ff34fce19d6b804eff5a3f57',
			},
			session_id: 'ses-ea7792a26f405e2ae9c6f49ca93bbe60',
			expectedOwner: {
				kind: 'scheduled_tool_run',
				toolRunId: 'toolrun-6b86b273ff34fce19d6b804eff5a3f57',
			},
			kind: 'scheduled_confirm',
		},
	])(
		'maps explicit owner routes and optional session context',
		({ owner, session_id, expectedOwner, kind }) => {
			const event = mapAppEvent({
				event: 'interaction:requested',
				id: 20,
				payload: {
					request_id: 'conf-4c1029697ee358715d3a14a2add817c4',
					...(session_id ? { session_id } : {}),
					owner,
					kind,
					status: 'pending',
					created_at: '2026-01-01T00:00:00Z',
					expires_at: '2026-01-01T00:05:00Z',
				},
			});

			expect(event?.payload).toMatchObject({
				requestId: 'conf-4c1029697ee358715d3a14a2add817c4',
				owner: expectedOwner,
				kind,
			});
			if (session_id) expect(event?.payload).toMatchObject({ sessionId: session_id });
			else expect(event?.payload).not.toHaveProperty('sessionId');
		},
	);

	it.each([
		{ kind: 'ask', owner: { kind: 'app_command' } },
		{
			kind: 'confirm',
			owner: {
				kind: 'scheduled_tool_run',
				tool_run_id: 'toolrun-6b86b273ff34fce19d6b804eff5a3f57',
			},
		},
		{
			kind: 'scheduled_confirm',
			owner: { kind: 'session', session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57' },
			session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57',
		},
		{ kind: 'scheduled_confirm', owner: { kind: 'app_command' } },
	])(
		'rejects an interaction kind that does not belong to its owner: $kind',
		({ kind, owner, session_id }) => {
			const event = mapAppEvent({
				event: 'interaction:requested',
				id: 23,
				payload: {
					request_id: 'interaction-owner-kind-mismatch',
					...(session_id ? { session_id } : {}),
					owner,
					kind,
					status: 'pending',
					options: [],
					created_at: '2026-01-01T00:00:00Z',
					expires_at: '2026-01-01T00:05:00Z',
				},
			});
			expect(event).toBeNull();
		},
	);

	it.each([undefined, 'not-a-deadline'])(
		'rejects pending confirmations without a valid owner deadline',
		(expiresAt) => {
			const event = mapAppEvent({
				event: 'interaction:requested',
				id: 22,
				payload: {
					request_id: 'conf-1fd4027863da5bf37b5f51f9be35d18c',
					session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57',
					owner: { kind: 'session', session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57' },
					kind: 'confirm',
					status: 'pending',
					created_at: '2026-01-01T00:00:00Z',
					...(expiresAt === undefined ? {} : { expires_at: expiresAt }),
				},
			});
			expect(event).toBeNull();
		},
	);

	it.each([
		{ owner: undefined, session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57' },
		{
			owner: { kind: 'session', session_id: 'ses-d9298a10d1b0735837dc4bd85dac641b' },
			session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57',
		},
		{ owner: { kind: 'session' }, session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57' },
		{ owner: { kind: 'scheduled_tool_run' }, session_id: undefined },
		{ owner: { kind: 'app_command' }, session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57' },
		{
			owner: {
				kind: 'future_owner',
				tool_run_id: 'toolrun-6b86b273ff34fce19d6b804eff5a3f57',
			},
			session_id: undefined,
		},
	])('rejects an invalid owner/context pair', ({ owner, session_id }) => {
		const event = mapAppEvent({
			event: 'interaction:requested',
			id: 21,
			payload: {
				request_id: 'conf-55839c1aeb493986bcea56970d1e88e9',
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
				request_id: 'conf-6b86b273ff34fce19d6b804eff5a3f57',
				session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57',
				owner: { kind: 'session', session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57' },
				kind: 'confirm',
				status: 'pending',
				created_at: 'now',
				options: ['okay', 1],
			},
		},
		{
			event: 'interaction:requested',
			payload: {
				request_id: 'conf-6b86b273ff34fce19d6b804eff5a3f57',
				session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57',
				owner: { kind: 'session', session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57' },
				kind: 'confirm',
				status: 'pending',
				created_at: 'now',
				summary: null,
			},
		},
		{
			event: 'interaction:requested',
			payload: {
				request_id: 'conf-6b86b273ff34fce19d6b804eff5a3f57',
				session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57',
				owner: { kind: 'session', session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57' },
				kind: 'confirm',
				status: 'pending',
				created_at: 'now',
				tool_index: null,
			},
		},
		{
			event: 'interaction:requested',
			payload: {
				request_id: 'conf-6b86b273ff34fce19d6b804eff5a3f57',
				session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57',
				owner: { kind: 'session', session_id: 'ses-6b86b273ff34fce19d6b804eff5a3f57' },
				kind: 'confirm',
				status: 'pending',
				created_at: 'now',
				tool_index: -1,
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
		for (const id of [Number.NaN, -1, 1.5, Number.MAX_SAFE_INTEGER + 1]) {
			expect(mapAppEvent({ event: 'hotkey:rebind', id, payload: {} })).toBeNull();
		}
		expect(mapAppEvent({ event: 'unknown:event', id: 1, payload: {} })).toBeNull();
		expect(mapAppEvent({ event: 'hotkey:rebind', id: 1, payload: null })).toBeNull();
	});
});
