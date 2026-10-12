import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('./tauri.ts', () => ({
	invoke: vi.fn().mockResolvedValue(undefined),
}));

import { invoke } from './tauri.ts';
import { openExternalTarget } from './externalCommands.ts';

describe('externalCommands', () => {
	beforeEach(() => {
		vi.clearAllMocks();
	});

	it('owns the open_external command name and generated request shape', async () => {
		await openExternalTarget({ target: 'https://example.com' });
		expect(invoke).toHaveBeenCalledWith('open_external', { target: 'https://example.com' });
	});
});
