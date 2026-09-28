import { beforeEach, describe, expect, it, vi } from 'vitest';
import { loadSettings } from './settingsCommand.ts';

vi.mock('$lib/tauri.ts', () => ({ invoke: vi.fn() }));

import { invoke } from '$lib/tauri.ts';

const invokeMock = vi.mocked(invoke);

describe('loadSettings', () => {
	beforeEach(() => invokeMock.mockReset());

	it('uses one command boundary and preserves future fields and enum values', async () => {
		const payload = {
			hotkey: { key_binding: 'Ctrl+Alt+H', mode: 'future_mode' },
			llm: { models: [], future_llm_field: { enabled: true } },
			future_settings_field: 'kept',
		};
		invokeMock.mockResolvedValue(payload as never);

		await expect(loadSettings()).resolves.toBe(payload);
		expect(invokeMock).toHaveBeenCalledOnce();
		expect(invokeMock).toHaveBeenCalledWith('get_settings');
	});

	it('preserves the existing no-op result for malformed root values', async () => {
		invokeMock.mockResolvedValue(null);
		await expect(loadSettings()).resolves.toBeNull();

		invokeMock.mockResolvedValue(['not', 'settings'] as never);
		await expect(loadSettings()).resolves.toBeNull();
	});
});
