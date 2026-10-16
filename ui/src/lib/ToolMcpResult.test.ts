import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, waitFor } from '@testing-library/svelte';
import ToolMcpResult from './ToolMcpResult.svelte';

const commands = vi.hoisted(() => ({
	getSandboxOutputReview: vi.fn(),
	discardSandboxOutputReview: vi.fn(),
	pickSandboxOutputDestination: vi.fn(),
	promoteSandboxOutput: vi.fn(),
}));

vi.mock('./toolsCommands.ts', () => commands);

describe('ToolMcpResult', () => {
	beforeEach(() => {
		vi.clearAllMocks();
		commands.getSandboxOutputReview.mockResolvedValue(null);
	});

	it('looks up the output review by the MCP tool step identity', async () => {
		const sessionId = 'ses-00000000000000000000000000000002';
		const sourceStepId = 'step-00000000000000000000000000000001';
		render(ToolMcpResult, {
			data: { result: 'done' },
			sessionId,
			sourceStepId,
		});

		await waitFor(() =>
			expect(commands.getSandboxOutputReview).toHaveBeenCalledWith({
				sessionId,
				sourceStepId,
			}),
		);
	});
});
