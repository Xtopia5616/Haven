import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, waitFor } from '@testing-library/svelte';
import ToolNestedReviewResult from './ToolNestedReviewResult.svelte';

const commands = vi.hoisted(() => ({
	getSandboxOutputReview: vi.fn(),
	discardSandboxOutputReview: vi.fn(),
	pickSandboxOutputDestination: vi.fn(),
	promoteSandboxOutput: vi.fn(),
}));

vi.mock('./toolsCommands.ts', () => commands);

describe('ToolNestedReviewResult', () => {
	beforeEach(() => {
		vi.clearAllMocks();
		commands.getSandboxOutputReview.mockResolvedValue(null);
	});

	it('loads each child review by its canonical child step identity', async () => {
		const sessionId = 'ses-00000000000000000000000000000002';
		const firstStepId = 'step-00000000000000000000000000000001';
		const secondStepId = 'step-00000000000000000000000000000003';
		render(ToolNestedReviewResult, {
			data: { status: 'completed' },
			sessionId,
			reviewStepIds: [firstStepId, secondStepId],
		});

		await waitFor(() => {
			expect(commands.getSandboxOutputReview).toHaveBeenCalledWith({ sessionId, sourceStepId: firstStepId });
			expect(commands.getSandboxOutputReview).toHaveBeenCalledWith({ sessionId, sourceStepId: secondStepId });
		});
	});
});
