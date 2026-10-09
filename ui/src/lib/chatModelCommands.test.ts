import { beforeEach, describe, expect, it, vi } from 'vitest';
import { setReasoningEffort, setWebSearch, switchModel } from './chatModelCommands.ts';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock('$lib/tauri.ts', () => ({ invoke }));

describe('chat model command boundary', () => {
	beforeEach(() => invoke.mockReset());

	it('switches the selected Chat profile', async () => {
		const request = { requestKind: 'chat', modelConfigId: 'chat-profile' } as const;
		invoke.mockResolvedValue(undefined);

		await expect(switchModel(request)).resolves.toBeUndefined();
		expect(invoke).toHaveBeenCalledWith('switch_model', request);
	});

	it('sets the Chat reasoning effort override', async () => {
		const request = { requestKind: 'chat', effort: 'high' } as const;
		invoke.mockResolvedValue(undefined);

		await expect(setReasoningEffort(request)).resolves.toBeUndefined();
		expect(invoke).toHaveBeenCalledWith('set_reasoning_effort', request);
	});

	it('sets the Chat web-search mode', async () => {
		const request = { requestKind: 'chat', mode: 'auto' } as const;
		invoke.mockResolvedValue(undefined);

		await expect(setWebSearch(request)).resolves.toBeUndefined();
		expect(invoke).toHaveBeenCalledWith('set_web_search', request);
	});
});
