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
	actionEventListeners,
	recordingEventListeners,
	registerListeners,
	registerOne,
	registerSessionListener,
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
		const unlisteners = [vi.fn(), vi.fn(), vi.fn()];
		mocks.listen
			.mockResolvedValueOnce(unlisteners[0])
			.mockResolvedValueOnce(unlisteners[1])
			.mockResolvedValueOnce(unlisteners[2]);

		const handlerA = vi.fn();
		const handlerB = vi.fn();
		const regs = registerListeners({
			'session:created': handlerA,
			'session:updated': handlerB,
		});
		await regs.ready;

		expect(mocks.listen).toHaveBeenCalledTimes(2);
		expect(mocks.listen).toHaveBeenNthCalledWith(1, 'session:created', handlerA);
		expect(mocks.listen).toHaveBeenNthCalledWith(2, 'session:updated', handlerB);

		regs.dispose();
		expect(unlisteners[0]).toHaveBeenCalledTimes(1);
		expect(unlisteners[1]).toHaveBeenCalledTimes(1);
	});

	it('logs registration failures and never throws', async () => {
		mocks.listen.mockRejectedValueOnce(new Error('boom'));

		const regs = registerListeners({ 'session:created': vi.fn() }, { tag: '+page' });
		await regs.ready; // must not reject

		expect(mocks.error).toHaveBeenCalledWith(
			'+page',
			expect.stringContaining('session:created'),
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

	it('maps the lifecycle wire DTO before calling chat event handlers', () => {
		const handler = vi.fn();
		const listeners = sessionEventListeners({ 'session:updated': handler });

		listeners['session:updated']({
			event: 'session:updated',
			id: 4,
			payload: {
				session_id: 'ses-1',
				status: 'paused',
				title: null,
				waiting_reason: 'user_input',
			},
		} as never);

		expect(handler).toHaveBeenCalledWith({
			event: 'session:updated',
			id: 4,
			payload: {
				sessionId: 'ses-1',
				status: 'paused',
				waitingReason: 'user_input',
				title: null,
				reason: null,
			},
		});
	});

	it('drops malformed lifecycle events before they reach handlers', () => {
		const handler = vi.fn();
		const listeners = sessionEventListeners({ 'session:updated': handler });

		listeners['session:updated']({
			event: 'session:updated',
			id: 5,
			payload: { status: 'running', title: null },
		} as never);

		expect(handler).not.toHaveBeenCalled();
		expect(mocks.warn).toHaveBeenCalledWith(
			'events',
			expect.stringContaining("Dropping malformed payload for 'session:updated'"),
		);
	});

	it('uses the same mapper for one-off typed session listeners', async () => {
		const handler = vi.fn();
		mocks.listen.mockResolvedValueOnce(vi.fn());

		await registerSessionListener('session:title-updated', handler);
		const rawListener = mocks.listen.mock.calls[0][1];
		rawListener({
			event: 'session:title-updated',
			id: 6,
			payload: { session_id: 'ses-1', title: 'A title' },
		});

		expect(handler).toHaveBeenCalledWith({
			event: 'session:title-updated',
			id: 6,
			payload: { sessionId: 'ses-1', title: 'A title' },
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

		const reg = await registerOne('session:title-updated', vi.fn(), { tag: 'memory' });
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
		await registerOne('session:updated', handler);
		const captured = mocks.listen.mock.calls[0][1];
		captured(event({ status: 'paused' }));
		expect(handler).toHaveBeenCalledWith(
			expect.objectContaining({ payload: { status: 'paused' } }),
		);
	});
});

describe('actionEventListeners', () => {
	beforeEach(() => {
		mocks.warn.mockReset();
	});

	it('maps the Rust wire payload before invoking the handler', () => {
		const handler = vi.fn();
		const listeners = actionEventListeners({ 'action:finished': handler });

		listeners['action:finished']({
			event: 'action:finished',
			id: 3,
			payload: {
				id: 'act-1',
				kind: 'background',
				session_id: 'ses-1',
				exit_code: 0,
			},
		} as never);

		expect(handler).toHaveBeenCalledWith({
			event: 'action:finished',
			id: 3,
			payload: {
				id: 'act-1',
				kind: 'background',
				sessionId: 'ses-1',
				exitCode: 0,
			},
		});
	});

	it('drops malformed action payloads with a warning that omits their contents', () => {
		const handler = vi.fn();
		const listeners = actionEventListeners({ 'action:finished': handler });
		const privateValue = 'payload-secret-value';

		listeners['action:finished']({
			event: 'action:finished',
			id: 4,
			payload: { kind: 'background', error: privateValue },
		} as never);

		expect(handler).not.toHaveBeenCalled();
		expect(mocks.warn).toHaveBeenCalledWith(
			'events',
			expect.stringContaining("Dropping malformed payload for 'action:finished'"),
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
