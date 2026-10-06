import { describe, it, expect, vi, beforeEach } from 'vitest';

const mocks = vi.hoisted(() => ({
	listen: vi.fn(),
	error: vi.fn(),
	warn: vi.fn(),
}));

vi.mock('./tauri.ts', () => ({
	listen: mocks.listen,
}));

vi.mock('./logger.ts', () => ({
	default: { error: mocks.error, warn: mocks.warn, info: vi.fn(), debug: vi.fn() },
}));

import {
	toolRunEventListeners,
	agentEventListeners,
	appEventListeners,
	recordingEventListeners,
	registerListeners,
	registerAppListener,
	registerOne,
	registerSessionLifecycleListener,
	sessionEventListeners,
} from './events.ts';

const event = (payload: any) => ({ payload });

describe('registerListeners', () => {
	beforeEach(() => {
		mocks.listen.mockReset();
		mocks.error.mockReset();
		mocks.warn.mockReset();
	});

	it('registers every event and disposes in registration order', async () => {
		const unlisteners = [vi.fn(), vi.fn()];
		mocks.listen
			.mockResolvedValueOnce(unlisteners[0])
			.mockResolvedValueOnce(unlisteners[1])
			.mockResolvedValueOnce(unlisteners[2]);

		const handlerA = vi.fn();
		const handlerB = vi.fn();
		const regs = registerListeners({
			'session:lifecycle': handlerA,
			'agent:thought': handlerB,
		});
		await regs.ready;

		expect(mocks.listen).toHaveBeenCalledTimes(2);
		expect(mocks.listen).toHaveBeenNthCalledWith(1, 'session:lifecycle', handlerA);
		expect(mocks.listen).toHaveBeenNthCalledWith(2, 'agent:thought', handlerB);

		regs.dispose();
		expect(unlisteners[0]).toHaveBeenCalledTimes(1);
		expect(unlisteners[1]).toHaveBeenCalledTimes(1);
	});

	it('logs registration failures and never throws', async () => {
		mocks.listen.mockRejectedValueOnce(new Error('boom'));

		const regs = registerListeners({ 'session:lifecycle': vi.fn() }, { tag: '+page' });
		await regs.ready; // must not reject

		expect(mocks.error).toHaveBeenCalledWith(
			'+page',
			expect.stringContaining('session:lifecycle'),
			expect.any(Error),
		);
		// dispose after a failed registration is a no-op, not a throw.
		expect(() => regs.dispose()).not.toThrow();
	});

	it('dispose before a pending listen resolves still cleans up on resolution', async () => {
		/** @type {((unsub: () => void) => void) | undefined} */
		let resolveListen: any;
		mocks.listen.mockReturnValue(new Promise((r) => (resolveListen = r)));

		const regs = registerListeners({ 'agent:thought': vi.fn() });
		regs.dispose();
		const unsub = vi.fn();
		resolveListen(unsub);
		await regs.ready;

		// The late-resolving unlisten is invoked immediately rather than leaked.
		expect(unsub).toHaveBeenCalledTimes(1);
	});
});

describe('sessionEventListeners', () => {
	beforeEach(() => {
		mocks.listen.mockReset();
		mocks.error.mockReset();
		mocks.warn.mockReset();
	});

	it('maps the single lifecycle event into a typed camelCase payload', () => {
		const handler = vi.fn();
		const listeners = sessionEventListeners(handler);

		listeners['session:lifecycle']({
			event: 'session:lifecycle',
			id: 4,
			payload: {
				type: 'updated',
				session_id: 'ses-1',
				status: 'paused',
				title: 'A title',
				waiting_reason: 'user_input',
				reason: 'Waiting for input',
			},
		} as never);

		expect(handler).toHaveBeenCalledWith({
			event: 'session:lifecycle',
			id: 4,
			payload: {
				type: 'updated',
				sessionId: 'ses-1',
				status: 'paused',
				waitingReason: 'user_input',
				title: 'A title',
				reason: 'Waiting for input',
			},
		});
	});

	it('keeps completion and error details in their discriminated terminal payloads', () => {
		const handler = vi.fn();
		const listeners = sessionEventListeners(handler);

		listeners['session:lifecycle']({
			event: 'session:lifecycle',
			id: 10,
			payload: {
				type: 'completed',
				session_id: 'ses-done',
				title: 'A title',
				reason: 'Finished',
			},
		} as never);
		listeners['session:lifecycle']({
			event: 'session:lifecycle',
			id: 11,
			payload: {
				type: 'error',
				session_id: 'ses-error',
				title: 'Build',
				error: 'Request failed',
			},
		} as never);

		expect(handler.mock.calls.map(([value]) => value.payload)).toEqual([
			{ type: 'completed', sessionId: 'ses-done', title: 'A title', reason: 'Finished' },
			{ type: 'error', sessionId: 'ses-error', title: 'Build', error: 'Request failed' },
		]);
	});

	it('rejects terminal states disguised as ordinary updates', () => {
		const handler = vi.fn();
		const listeners = sessionEventListeners(handler);

		listeners['session:lifecycle']({
			event: 'session:lifecycle',
			id: 5,
			payload: {
				type: 'updated',
				session_id: 'ses-1',
				status: 'completed',
				title: 'A title',
			},
		} as never);

		expect(handler).not.toHaveBeenCalled();
		expect(mocks.warn).toHaveBeenCalledWith(
			'events',
			expect.stringContaining("Dropping malformed payload for 'session:lifecycle'"),
		);
	});

	it('uses the same mapper for the one-off lifecycle listener', async () => {
		const handler = vi.fn();
		mocks.listen.mockResolvedValueOnce(vi.fn());

		await registerSessionLifecycleListener(handler);
		expect(mocks.listen).toHaveBeenCalledWith('session:lifecycle', expect.any(Function));
		const rawListener = mocks.listen.mock.calls[0][1];
		rawListener({
			event: 'session:lifecycle',
			id: 6,
			payload: { type: 'title_updated', session_id: 'ses-1', title: 'A title' },
		});

		expect(handler).toHaveBeenCalledWith({
			event: 'session:lifecycle',
			id: 6,
			payload: { type: 'title_updated', sessionId: 'ses-1', title: 'A title' },
		});
	});
});

describe('registerOne', () => {
	beforeEach(() => {
		mocks.listen.mockReset();
		mocks.error.mockReset();
	});

	it('returns a dispose handle that unregisters', async () => {
		const unsub = vi.fn();
		mocks.listen.mockResolvedValueOnce(unsub);

		const reg = await registerOne('session:lifecycle', vi.fn(), { tag: 'memory' });
		expect(mocks.listen).toHaveBeenCalledTimes(1);
		reg.dispose();
		expect(unsub).toHaveBeenCalledTimes(1);
	});

	it('returns a no-op handle when registration fails', async () => {
		mocks.listen.mockRejectedValueOnce(new Error('boom'));

		const reg = await registerOne('mcp:status_change', vi.fn(), { tag: 'tools' });
		expect(mocks.error).toHaveBeenCalledTimes(1);
		expect(() => reg.dispose()).not.toThrow();
	});

	it('forwards events to the handler', async () => {
		const handler = vi.fn();
		mocks.listen.mockResolvedValueOnce(vi.fn());
		await registerOne('session:lifecycle', handler);
		const captured = mocks.listen.mock.calls[0][1];
		captured(event({ status: 'paused' }));
		expect(handler).toHaveBeenCalledWith(
			expect.objectContaining({ payload: { status: 'paused' } }),
		);
	});
});

describe('appEventListeners', () => {
	beforeEach(() => {
		mocks.warn.mockReset();
	});

	it('maps app events before calling shared shell handlers', () => {
		const handler = vi.fn();
		const listeners = appEventListeners({ 'hotkey:rebind': handler });

		listeners['hotkey:rebind']({
			event: 'hotkey:rebind',
			id: 7,
			payload: { old_binding: 'Ctrl+A', new_binding: 'Ctrl+B' },
		} as never);

		expect(handler).toHaveBeenCalledWith({
			event: 'hotkey:rebind',
			id: 7,
			payload: { oldBinding: 'Ctrl+A', newBinding: 'Ctrl+B' },
		});
	});

	it('drops malformed app payloads with a payload-free warning', () => {
		const handler = vi.fn();
		const listeners = appEventListeners({ 'hotkey:rebind': handler });
		const payload = {
			old_binding: 17,
			new_binding: 'Ctrl+B',
			private_value: 'must not be logged',
		};

		listeners['hotkey:rebind']({
			event: 'hotkey:rebind',
			id: 7,
			payload,
		} as never);

		expect(handler).not.toHaveBeenCalled();
		expect(mocks.warn).toHaveBeenCalledWith(
			'events',
			expect.stringContaining("Dropping malformed payload for 'hotkey:rebind'"),
		);
		expect(JSON.stringify(mocks.warn.mock.calls)).not.toContain('must not be logged');
	});
});

describe('registerAppListener', () => {
	beforeEach(() => {
		mocks.listen.mockReset();
		mocks.error.mockReset();
		mocks.warn.mockReset();
	});

	it('maps one-off app listeners through the same contract boundary', async () => {
		const handler = vi.fn();
		mocks.listen.mockResolvedValueOnce(vi.fn());

		await registerAppListener('hotkey:rebind', handler, { tag: 'tools' });
		const rawListener = mocks.listen.mock.calls[0][1];
		rawListener({
			event: 'hotkey:rebind',
			id: 8,
			payload: {
				old_binding: 'Ctrl+Shift+Space',
				new_binding: 'Ctrl+Alt+Space',
				future_field: 'not part of the view DTO',
			},
		});

		expect(handler).toHaveBeenCalledWith({
			event: 'hotkey:rebind',
			id: 8,
			payload: {
				oldBinding: 'Ctrl+Shift+Space',
				newBinding: 'Ctrl+Alt+Space',
			},
		});
	});

	it('validates known app status variants and ignores additive fields', async () => {
		const handler = vi.fn();
		mocks.listen.mockResolvedValueOnce(vi.fn());

		await registerAppListener('mcp:status_change', handler, { tag: 'tools' });
		const rawListener = mocks.listen.mock.calls[0][1];
		const payload = {
			name: 'server',
			status: { Offline: { error: 'not available' } },
			future_field: 'preserved',
		};
		rawListener({ event: 'mcp:status_change', id: 9, payload });

		expect(handler).toHaveBeenCalledWith({
			event: 'mcp:status_change',
			id: 9,
			payload: { name: 'server', status: { Offline: { error: 'not available' } } },
		});
	});

	it('drops malformed one-off app events before invoking handlers', async () => {
		const handler = vi.fn();
		mocks.listen.mockResolvedValueOnce(vi.fn());

		await registerAppListener('interaction:requested', handler, { tag: 'layout' });
		const rawListener = mocks.listen.mock.calls[0][1];
		rawListener({
			event: 'interaction:requested',
			id: 10,
			payload: { id: 'conf-1', session_id: 'ses-1', kind: 'confirm' },
		});

		expect(handler).not.toHaveBeenCalled();
		expect(mocks.warn).toHaveBeenCalledWith(
			'events',
			expect.stringContaining("Dropping malformed payload for 'interaction:requested'"),
		);
	});
});

describe('toolRunEventListeners', () => {
	beforeEach(() => {
		mocks.warn.mockReset();
	});

	it('maps the Rust wire payload before invoking the handler', () => {
		const handler = vi.fn();
		const listeners = toolRunEventListeners({ 'tool_run:finished': handler });

		listeners['tool_run:finished']({
			event: 'tool_run:finished',
			id: 3,
			payload: {
				id: 'toolrun-1',
				kind: 'background',
				session_id: 'ses-1',
				exit_code: 0,
			},
		} as never);

		expect(handler).toHaveBeenCalledWith({
			event: 'tool_run:finished',
			id: 3,
			payload: {
				id: 'toolrun-1',
				kind: 'background',
				sessionId: 'ses-1',
				exitCode: 0,
			},
		});
	});

	it('drops malformed ToolRun payloads with a warning that omits their contents', () => {
		const handler = vi.fn();
		const listeners = toolRunEventListeners({ 'tool_run:finished': handler });
		const privateValue = 'payload-secret-value';

		listeners['tool_run:finished']({
			event: 'tool_run:finished',
			id: 4,
			payload: { kind: 'background', error: privateValue },
		} as never);

		expect(handler).not.toHaveBeenCalled();
		expect(mocks.warn).toHaveBeenCalledWith(
			'events',
			expect.stringContaining("Dropping malformed payload for 'tool_run:finished'"),
		);
		expect(JSON.stringify(mocks.warn.mock.calls)).not.toContain(privateValue);
	});
});

describe('agentEventListeners', () => {
	beforeEach(() => {
		mocks.warn.mockReset();
		mocks.error.mockReset();
	});

	it('maps each arrival before dispatch and preserves arrival order', () => {
		const received: string[] = [];
		const listeners = agentEventListeners({
			'agent:thought': (event) =>
				received.push(`${event.payload.sessionId}:${event.payload.thought}`),
		});
		const arrivals = [
			{
				session_id: 'ses-1',
				thought: 'first',
				step_number: 1,
				run_id: 1,
				message_id: 'step-1',
			},
			{
				session_id: 'ses-1',
				thought: 'second',
				step_number: 2,
				run_id: 1,
				message_id: 'step-2',
			},
		];

		arrivals.forEach((payload, id) => {
			listeners['agent:thought']({ event: 'agent:thought', id, payload } as never);
		});

		expect(received).toEqual(['ses-1:first', 'ses-1:second']);
	});

	it('drops malformed payloads with a generic warning that omits payload content', () => {
		const handler = vi.fn();
		const listeners = agentEventListeners({ 'agent:thought': handler });
		const privateValue = 'sensitive-thought-content';

		listeners['agent:thought']({
			event: 'agent:thought',
			id: 4,
			payload: {
				session_id: 'ses-1',
				thought: privateValue,
				step_number: 'bad',
				run_id: 2,
				message_id: 'step-1',
			},
		} as never);

		expect(handler).not.toHaveBeenCalled();
		expect(mocks.warn).toHaveBeenCalledWith(
			'events',
			expect.stringContaining("Dropping malformed payload for 'agent:thought'"),
		);
		expect(JSON.stringify(mocks.warn.mock.calls)).not.toContain(privateValue);
	});
});

describe('recordingEventListeners', () => {
	it('maps each received recording event before invoking handlers and keeps arrival order', () => {
		const received: string[] = [];
		const listeners = recordingEventListeners({
			'recording:started': (event) => {
				received.push(`${event.event}:${event.payload.sessionId}`);
			},
			'recording:stopped': (event) => {
				received.push(`${event.event}:${event.payload.reason}`);
			},
			'transcription:started': (event) => {
				received.push(`${event.event}:${event.payload.sessionId}`);
			},
			'transcription:result': (event) => {
				received.push(`${event.event}:${event.payload.text}`);
			},
		});

		const arrivals = [
			{
				event: 'recording:started',
				payload: { is_recording: true, session_id: 'rec-1' },
			},
			{
				event: 'recording:stopped',
				payload: { is_recording: false, reason: 'silence' },
			},
			{ event: 'transcription:started', payload: { session_id: 'rec-1' } },
			{
				event: 'transcription:result',
				payload: { session_id: 'rec-1', text: 'hello', duration_ms: 800 },
			},
		] as const;

		arrivals.forEach((arrival, id) => {
			listeners[arrival.event]({ ...arrival, id } as never);
		});

		expect(received).toEqual([
			'recording:started:rec-1',
			'recording:stopped:silence',
			'transcription:started:rec-1',
			'transcription:result:hello',
		]);
	});
});
