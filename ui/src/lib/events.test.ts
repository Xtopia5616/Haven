import { describe, it, expect, vi, beforeEach } from 'vitest';

const mocks = vi.hoisted(() => ({
	listen: vi.fn(),
	error: vi.fn(),
}));

vi.mock('./tauri.ts', () => ({
	listen: mocks.listen,
}));

vi.mock('./logger.ts', () => ({
	default: { error: mocks.error, warn: vi.fn(), info: vi.fn(), debug: vi.fn() },
}));

import { registerListeners, registerOne } from './events.ts';

const event = (payload) => ({ payload });

describe('registerListeners', () => {
	beforeEach(() => {
		mocks.listen.mockReset();
		mocks.error.mockReset();
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
			expect.any(Error)
		);
		// dispose after a failed registration is a no-op, not a throw.
		expect(() => regs.dispose()).not.toThrow();
	});

	it('dispose before a pending listen resolves still cleans up on resolution', async () => {
		/** @type {(unsub: () => void) => void} */
		let resolveListen;
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

describe('registerOne', () => {
	beforeEach(() => {
		mocks.listen.mockReset();
		mocks.error.mockReset();
	});

	it('returns a dispose handle that unregisters', async () => {
		const unsub = vi.fn();
		mocks.listen.mockResolvedValueOnce(unsub);

		const reg = await registerOne('session:title-updated', vi.fn(), { tag: 'history' });
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
		expect(handler).toHaveBeenCalledWith(expect.objectContaining({ payload: { status: 'paused' } }));
	});
});
