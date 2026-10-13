import { describe, it, expect, vi, beforeEach } from 'vitest';
import { invoke, listen, isTauri } from './tauri.ts';
import type { TauriCommandName } from './contracts/generatedCommands.ts';

const mocks = vi.hoisted(() => ({ logError: vi.fn() }));

vi.mock('./errorHandling.ts', () => ({ logError: mocks.logError }));

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }));

import { invoke as rawInvoke } from '@tauri-apps/api/core';
import { listen as rawListen } from '@tauri-apps/api/event';

describe('tauri.ts in a non-Tauri environment', () => {
	beforeEach(() => {
		const w = window as any;
		delete w.__TAURI_INTERNALS__;
		delete w.__TAURI__;
		vi.mocked(rawInvoke).mockReset();
		vi.mocked(rawListen).mockReset();
		mocks.logError.mockReset();
	});

	it('isTauri is false without Tauri globals', () => {
		expect(isTauri()).toBe(false);
	});

	it('invoke rejects with a helpful error', async () => {
		await expect(invoke('some_command' as TauriCommandName, { a: 1 } as never)).rejects.toThrow(
			"Tauri not available, cannot invoke 'some_command'",
		);
	});

	it('rejects command values that do not match the generated Rust response shape', async () => {
		const w = window as any;
		w.__TAURI_INTERNALS__ = {};
		vi.mocked(rawInvoke).mockResolvedValueOnce('false');

		await expect(invoke('is_autostart_enabled')).rejects.toThrow(
			"Invalid Tauri response for 'is_autostart_enabled' at $",
		);
	});

	it('normalizes primitive invoke rejections after logging them', async () => {
		const w = window as any;
		w.__TAURI_INTERNALS__ = {};
		vi.mocked(rawInvoke).mockRejectedValueOnce('backend failure');

		await expect(invoke('is_autostart_enabled')).rejects.toThrow('backend failure');
	});

	it('listen returns a no-op unsubscribe function', async () => {
		const unsubscribe = await listen('some:event', vi.fn());
		expect(typeof unsubscribe).toBe('function');
		// Calling it must not throw.
		expect(unsubscribe()).toBeUndefined();
	});

	it('propagates listener registration failures for the event owner to log', async () => {
		const w = window as any;
		w.__TAURI_INTERNALS__ = {};
		vi.mocked(rawListen).mockRejectedValueOnce(new Error('listener registration failed'));

		await expect(listen('some:event', vi.fn())).rejects.toThrow('listener registration failed');
		expect(mocks.logError).not.toHaveBeenCalled();
	});
});
