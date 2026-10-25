import { describe, expect, it } from 'vitest';
import { interactionOwnerToWire, mapAppEvent, parseConfirmationResolutionResult } from './app.ts';

describe('app-shell IPC contract', () => {
	it('maps MCP resource update events and rejects malformed URIs', () => {
		const event = mapAppEvent({
			event: 'mcp:resource_changed',
			id: 1,
			payload: { name: 'files', uri: 'mcp://files/notes.txt' },
		});
		expect(event?.payload).toEqual({ name: 'files', uri: 'mcp://files/notes.txt' });
		expect(
			mapAppEvent({
				event: 'mcp:resource_changed',
				id: 2,
				payload: { name: 'files', uri: 42 },
			}),
		).toBeNull();
	});

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
			{ kind: 'session', sessionId: 'ses-0ecf371384ab79d597760ca41f2ba21c' } as const,
			{ kind: 'session', session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c' },
		],
		[
			{
				kind: 'schedule_run',
				scheduleRunId: 'schrun-d409f06a31c9daee6d1a4f437a1f1f85',
			} as const,
			{ kind: 'schedule_run', schedule_run_id: 'schrun-d409f06a31c9daee6d1a4f437a1f1f85' },
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
				request_id: 'conf-79de9202e37eef0078c66a58ae861c3b',
				session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c',
				owner: { kind: 'session', session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c' },
				kind: 'confirm',
				status: 'pending',
				resource_boundary_extension: false,
				options: [],
				invocation_step_id: 'step-ea869e583dff7756d1747b32ad55e798',
				tool_index: 1,
				tool_call_id: 'call-1',
				tool_name: 'run_command',
				risk_level: 'high',
				created_at: '2026-01-01T00:00:00Z',
				expires_at: '2026-01-01T00:05:00Z',
				summary: '将执行一条受保护的本机命令；请检查下方调用参数。',
				permission_key: 'tool.run_command',
				tool_input: {
					command: 'cargo test --workspace',
					cwd: 'D:\\Workspace\\Haven',
				},
				future_field: 'not part of the renderer DTO',
			},
		});

		expect(event?.payload).toEqual({
			requestId: 'conf-79de9202e37eef0078c66a58ae861c3b',
			sessionId: 'ses-0ecf371384ab79d597760ca41f2ba21c',
			owner: { kind: 'session', sessionId: 'ses-0ecf371384ab79d597760ca41f2ba21c' },
			kind: 'confirm',
			status: 'pending',
			resourceBoundaryExtension: false,
			options: [],
			invocationStepId: 'step-ea869e583dff7756d1747b32ad55e798',
			toolIndex: 1,
			toolCallId: 'call-1',
			toolName: 'run_command',
			riskLevel: 'high',
			summary: '将执行一条受保护的本机命令；请检查下方调用参数。',
			permissionKey: 'tool.run_command',
			toolInput: {
				command: 'cargo test --workspace',
				cwd: 'D:\\Workspace\\Haven',
			},
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
				request_id: 'step-20edda8dd4d8ca425075d9951bd5e83c',
				session_id: 'ses-485b594d54374c140c71b794d06112bd',
				owner: { kind: 'session', session_id: 'ses-485b594d54374c140c71b794d06112bd' },
				kind: 'ask',
				status: 'pending',
				resource_boundary_extension: false,
				options: ['继续', '停止'],
				created_at: '2026-01-01T00:00:00Z',
			},
		});

		expect(event?.payload).toMatchObject({
			requestId: 'step-20edda8dd4d8ca425075d9951bd5e83c',
			kind: 'ask',
			owner: { kind: 'session', sessionId: 'ses-485b594d54374c140c71b794d06112bd' },
		});
	});

	it('does not accept invocation details on an Ask interaction', () => {
		const event = mapAppEvent({
			event: 'interaction:requested',
			id: 241,
			payload: {
				request_id: 'step-20edda8dd4d8ca425075d9951bd5e83c',
				session_id: 'ses-485b594d54374c140c71b794d06112bd',
				owner: { kind: 'session', session_id: 'ses-485b594d54374c140c71b794d06112bd' },
				kind: 'ask',
				status: 'pending',
				resource_boundary_extension: false,
				options: ['继续'],
				tool_input: { unexpected: true },
				created_at: '2026-01-01T00:00:00Z',
			},
		});

		expect(event).toBeNull();
	});

	it('does not accept invocation details after a confirmation is resolved', () => {
		const event = mapAppEvent({
			event: 'interaction:requested',
			id: 242,
			payload: {
				request_id: 'conf-20edda8dd4d8ca425075d9951bd5e83c',
				session_id: 'ses-485b594d54374c140c71b794d06112bd',
				owner: { kind: 'session', session_id: 'ses-485b594d54374c140c71b794d06112bd' },
				kind: 'confirm',
				status: 'resolved',
				resource_boundary_extension: false,
				options: [],
				invocation_step_id: 'step-20edda8dd4d8ca425075d9951bd5e83c',
				tool_index: 0,
				tool_name: 'shell',
				risk_level: 'high',
				summary: 'Run protected command',
				tool_input: { command: 'cargo test' },
				created_at: '2026-01-01T00:00:00Z',
				expires_at: '2026-01-01T00:05:00Z',
			},
		});

		expect(event).toBeNull();
	});

	it('rejects the retired generic interaction id field', () => {
		const event = mapAppEvent({
			event: 'interaction:requested',
			id: 25,
			payload: {
				id: 'conf-79de9202e37eef0078c66a58ae861c3b',
				session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c',
				owner: { kind: 'session', session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c' },
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
			request_id: 'conf-a51f4384e7b5b61dfce716dedff5b3fd',
			session_id: 'ses-103ac63490bc5f6c27fd45405efecd69',
			owner: { kind: 'session', session_id: 'ses-103ac63490bc5f6c27fd45405efecd69' },
			kind: 'confirm',
			status: 'pending',
			resource_boundary_extension: false,
			created_at: '2026-01-01T00:00:00Z',
			expires_at: '2026-01-01T00:05:00Z',
		};
		for (const payload of [
			{ ...confirmation, request_id: 'step-455572844fd881c7ae3ae7d45163af1a' },
			{ ...confirmation, session_id: 'msg-fc16f96c92a9c37a323bb7d0f47a4e22' },
			{ ...confirmation, invocation_step_id: 'msg-fc16f96c92a9c37a323bb7d0f47a4e22' },
			{
				...confirmation,
				owner: { kind: 'session', session_id: 'toolrun-58ec300e6432076c8ec176f56120ec01' },
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

	it('requires presentation fields that the interaction owner and kind promise', () => {
		const sessionConfirm = {
			request_id: 'conf-00000000000000000000000000000031',
			session_id: 'ses-00000000000000000000000000000031',
			owner: { kind: 'session', session_id: 'ses-00000000000000000000000000000031' },
			kind: 'confirm',
			status: 'pending',
			resource_boundary_extension: false,
			options: [],
			tool_name: 'run_command',
			risk_level: 'high',
			summary: 'Run a protected command',
			tool_index: 0,
			invocation_step_id: 'step-00000000000000000000000000000031',
			created_at: '2026-01-01T00:00:00Z',
			expires_at: '2026-01-01T00:05:00Z',
		};
		for (const field of [
			'tool_name',
			'risk_level',
			'summary',
			'tool_index',
			'invocation_step_id',
		]) {
			const incomplete = { ...sessionConfirm };
			delete incomplete[field as keyof typeof incomplete];
			expect(
				mapAppEvent({ event: 'interaction:requested', id: 40, payload: incomplete }),
			).toBeNull();
		}

		const scheduledConfirm = {
			request_id: 'conf-00000000000000000000000000000032',
			owner: {
				kind: 'schedule_run',
				schedule_run_id: 'schrun-00000000000000000000000000000032',
			},
			kind: 'scheduled_confirm',
			status: 'pending',
			resource_boundary_extension: false,
			options: [],
			tool_name: 'schedule',
			summary: 'Run scheduled task',
			permission_key: 'tool.schedule',
			created_at: '2026-01-01T00:00:00Z',
			expires_at: '2026-01-01T00:05:00Z',
		};
		for (const field of ['tool_name', 'summary', 'permission_key']) {
			const incomplete = { ...scheduledConfirm };
			delete incomplete[field as keyof typeof incomplete];
			expect(
				mapAppEvent({ event: 'interaction:requested', id: 41, payload: incomplete }),
			).toBeNull();
		}

		const {
			session_id: _sessionId,
			invocation_step_id: _invocationStepId,
			...confirmDetails
		} = sessionConfirm;
		const appCommandConfirm = {
			...confirmDetails,
			owner: { kind: 'app_command' },
		};
		expect(
			mapAppEvent({ event: 'interaction:requested', id: 42, payload: appCommandConfirm }),
		).not.toBeNull();
		expect(
			mapAppEvent({
				event: 'interaction:requested',
				id: 43,
				payload: {
					...appCommandConfirm,
					invocation_step_id: sessionConfirm.invocation_step_id,
				},
			}),
		).toBeNull();
	});

	it('rejects unknown interaction enum values while defaulting omitted options', () => {
		const event = mapAppEvent({
			event: 'interaction:requested',
			id: 2,
			payload: {
				request_id: 'interaction-1',
				session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c',
				owner: { kind: 'session', session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c' },
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
				kind: 'schedule_run',
				schedule_run_id: 'schrun-d409f06a31c9daee6d1a4f437a1f1f85',
			},
			expectedOwner: {
				kind: 'schedule_run',
				scheduleRunId: 'schrun-d409f06a31c9daee6d1a4f437a1f1f85',
			},
			kind: 'scheduled_confirm',
		},
		{
			owner: {
				kind: 'schedule_run',
				schedule_run_id: 'schrun-d409f06a31c9daee6d1a4f437a1f1f85',
			},
			session_id: 'ses-8012a8abdcc2d6c9fe75559e9bdcca24',
			expectedOwner: {
				kind: 'schedule_run',
				scheduleRunId: 'schrun-d409f06a31c9daee6d1a4f437a1f1f85',
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
					request_id: 'conf-e3ddd52c190a238a5980afa6555fe93e',
					...(session_id ? { session_id } : {}),
					owner,
					kind,
					status: 'pending',
					resource_boundary_extension: false,
					created_at: '2026-01-01T00:00:00Z',
					expires_at: '2026-01-01T00:05:00Z',
					...(kind === 'confirm'
						? {
								tool_name: 'run_command',
								risk_level: 'high',
								summary: 'Run a protected command',
								tool_index: 0,
							}
						: {
								tool_name: 'schedule',
								summary: 'Run a scheduled task',
								permission_key: 'tool.schedule',
							}),
				},
			});

			expect(event?.payload).toMatchObject({
				requestId: 'conf-e3ddd52c190a238a5980afa6555fe93e',
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
				kind: 'schedule_run',
				schedule_run_id: 'schrun-d409f06a31c9daee6d1a4f437a1f1f85',
			},
		},
		{
			kind: 'scheduled_confirm',
			owner: { kind: 'session', session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c' },
			session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c',
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
					request_id: 'conf-10916ee88684482ff20e3f8285e2f926',
					session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c',
					owner: { kind: 'session', session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c' },
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
		{ owner: undefined, session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c' },
		{
			owner: { kind: 'session', session_id: 'ses-71939af277fc2fa1f18590c5f15ae8df' },
			session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c',
		},
		{ owner: { kind: 'session' }, session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c' },
		{ owner: { kind: 'schedule_run' }, session_id: undefined },
		{ owner: { kind: 'app_command' }, session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c' },
		{
			owner: {
				kind: 'future_owner',
				schedule_run_id: 'schrun-d409f06a31c9daee6d1a4f437a1f1f85',
			},
			session_id: undefined,
		},
	])('rejects an invalid owner/context pair', ({ owner, session_id }) => {
		const event = mapAppEvent({
			event: 'interaction:requested',
			id: 21,
			payload: {
				request_id: 'conf-2b945745d4c4982ecd1e546098b729ef',
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
				request_id: 'conf-79de9202e37eef0078c66a58ae861c3b',
				session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c',
				owner: { kind: 'session', session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c' },
				kind: 'confirm',
				status: 'pending',
				created_at: 'now',
				options: ['okay', 1],
			},
		},
		{
			event: 'interaction:requested',
			payload: {
				request_id: 'conf-79de9202e37eef0078c66a58ae861c3b',
				session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c',
				owner: { kind: 'session', session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c' },
				kind: 'confirm',
				status: 'pending',
				created_at: 'now',
				summary: null,
			},
		},
		{
			event: 'interaction:requested',
			payload: {
				request_id: 'conf-79de9202e37eef0078c66a58ae861c3b',
				session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c',
				owner: { kind: 'session', session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c' },
				kind: 'confirm',
				status: 'pending',
				created_at: 'now',
				tool_index: null,
			},
		},
		{
			event: 'interaction:requested',
			payload: {
				request_id: 'conf-79de9202e37eef0078c66a58ae861c3b',
				session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c',
				owner: { kind: 'session', session_id: 'ses-0ecf371384ab79d597760ca41f2ba21c' },
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
