import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import SandboxOutputReviewPanel from './SandboxOutputReviewPanel.svelte';

const commands = vi.hoisted(() => ({
	getSandboxOutputReview: vi.fn(),
	discardSandboxOutputReview: vi.fn(),
	pickSandboxOutputDestination: vi.fn(),
	promoteSandboxOutput: vi.fn(),
}));

vi.mock('./toolsCommands.ts', () => commands);

const sessionId = 'ses-00000000000000000000000000000002';
const sourceStepId = 'step-00000000000000000000000000000001';

function makeReview(promoted = false) {
	return {
		sessionId,
		sourceStepId,
		expiresAtUnixMs: 1_800_000_000_000,
		storageChargeBytes: 128,
		changes: [
			{
				changeIndex: 0,
				relativePath: 'reports/result.txt',
				change: 'created' as const,
				beforeBytes: null,
				afterBytes: 12,
				beforeSha256: null,
				afterSha256: 'a'.repeat(64),
				promoted,
			},
			{
				changeIndex: 1,
				relativePath: 'config/changed.json',
				change: 'modified' as const,
				beforeBytes: 8,
				afterBytes: 10,
				beforeSha256: 'b'.repeat(64),
				afterSha256: 'c'.repeat(64),
				promoted: false,
			},
		],
	};
}

describe('SandboxOutputReviewPanel', () => {
	beforeEach(() => {
		vi.clearAllMocks();
		commands.getSandboxOutputReview.mockResolvedValue(makeReview());
		commands.discardSandboxOutputReview.mockResolvedValue(true);
		commands.pickSandboxOutputDestination.mockResolvedValue('C:\\Workspace\\result.txt');
		commands.promoteSandboxOutput.mockResolvedValue(undefined);
	});

	it('shows only review metadata and makes modified files view-only', async () => {
		render(SandboxOutputReviewPanel, { sessionId, sourceStepId });

		await screen.findByText('reports/result.txt');
		expect(screen.getByText('新增')).toBeTruthy();
		expect(screen.getByText('已修改')).toBeTruthy();
		expect(screen.getByText('仅供查看')).toBeTruthy();
		expect(screen.getAllByRole('button', { name: '选择位置并保存' })).toHaveLength(1);
		expect(screen.queryByText('bounded-review')).toBeNull();
	});

	it('queues one-time promotion and refreshes the promoted state', async () => {
		commands.getSandboxOutputReview
			.mockResolvedValueOnce(makeReview(false))
			.mockResolvedValueOnce(makeReview(true));
		commands.promoteSandboxOutput.mockRejectedValueOnce(
			new Error(JSON.stringify({ requires_confirmation: true, request_id: 'conf-123' })),
		);
		render(SandboxOutputReviewPanel, { sessionId, sourceStepId });

		await fireEvent.click(await screen.findByRole('button', { name: '选择位置并保存' }));
		await screen.findByText('请在权限窗口中处理一次性授权；处理后刷新此审阅状态。');
		expect(commands.pickSandboxOutputDestination).toHaveBeenCalledWith('reports/result.txt');
		expect(commands.promoteSandboxOutput).toHaveBeenCalledWith({
			sessionId,
			sourceStepId,
			changeIndex: 0,
			destination: 'C:\\Workspace\\result.txt',
		});

		await fireEvent.click(screen.getByRole('button', { name: '刷新状态' }));
		await screen.findByText('文件已创建到你选择的位置。');
		expect(screen.getByText('已保存')).toBeTruthy();
		expect(screen.queryByRole('button', { name: '选择位置并保存' })).toBeNull();
	});

	it('discards the matching review explicitly', async () => {
		render(SandboxOutputReviewPanel, { sessionId, sourceStepId });

		await fireEvent.click(await screen.findByRole('button', { name: '丢弃全部审阅数据' }));
		await waitFor(() => expect(commands.discardSandboxOutputReview).toHaveBeenCalledWith({
			sessionId,
			sourceStepId,
		}));
		await screen.findByText('临时审阅数据已丢弃。');
		expect(screen.queryByText('reports/result.txt')).toBeNull();
	});
});
