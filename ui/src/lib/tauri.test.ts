import { describe, it, expect, vi, beforeEach } from 'vitest';
import { invoke, listen } from './tauri.ts';

describe('tauri.ts in a non-Tauri environment', () => {
	beforeEach(() => {
		const w = window as any;
		delete w.__TAURI_INTERNALS__;
		delete w.__TAURI__;
	});

	it('invoke rejects with a helpful error', async () => {
		await expect(invoke('some_command', { a: 1 })).rejects.toThrow(
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
