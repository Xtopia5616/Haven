import { describe, it, expect, vi, beforeEach } from 'vitest';
import { invoke, listen, isTauri } from './tauri.ts';
import type { TauriCommandName } from './contracts/generatedCommands.ts';

describe('tauri.ts in a non-Tauri environment', () => {
	beforeEach(() => {
		const w = window as any;
		delete w.__TAURI_INTERNALS__;
		delete w.__TAURI__;
	});

	it('isTauri is false without Tauri globals', () => {
		expect(isTauri()).toBe(false);
	});

	it('invoke rejects with a helpful error', async () => {
		await expect(invoke('some_command' as TauriCommandName, { a: 1 } as never)).rejects.toThrow(
			"Tauri not available, cannot invoke 'some_command'",
		);
	});

	it('listen returns a no-op unsubscribe function', async () => {
		const unsubscribe = await listen('some:event', vi.fn());
		expect(typeof unsubscribe).toBe('function');
		// Calling it must not throw.
		expect(unsubscribe()).toBeUndefined();
	});
});
