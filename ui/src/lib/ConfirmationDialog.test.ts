import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import ConfirmationDialog from './ConfirmationDialog.svelte';

describe('ConfirmationDialog', () => {
	it('keeps one-time approval primary and moves broader grants into more options', async () => {
		const onConfirm = vi.fn();
		const { container } = render(ConfirmationDialog as any, {
			stepId: 'step-test',
			toolName: 'files.write',
			sessionId: 'ses-test',
			sessionTitle: '测试会话',
			riskLevel: 'medium',
			summary: '写入文件',
			permissionKey: 'files.write',
			onConfirm,
		});

		const onceButton = screen.getByRole('button', { name: '本次允许' });
		const moreAllowButton = screen.getByRole('button', { name: '更多允许' });
		expect(onceButton.classList.contains('md-btn--filled')).toBe(true);
		expect(moreAllowButton.classList.contains('md-btn--text')).toBe(true);
		expect(moreAllowButton.getAttribute('aria-haspopup')).toBe('menu');

		const allowButtonLabels = Array.from(container.querySelectorAll('.allow-group button')).map(
			(button) => button.textContent?.trim() || button.getAttribute('aria-label'),
		);
		expect(allowButtonLabels).toEqual(['本次允许', '本对话允许此操作', '更多允许']);
		expect(
			container
				.querySelector('.actions')
				?.firstElementChild?.classList.contains('allow-group'),
		).toBe(true);
		expect(
			container.querySelector('.actions')?.lastElementChild?.classList.contains('deny-split'),
		).toBe(true);

		await fireEvent.click(moreAllowButton);
		expect(screen.getByRole('menuitem', { name: '永久允许此操作' })).toBeTruthy();
		expect(moreAllowButton.getAttribute('aria-expanded')).toBe('true');

		await fireEvent.click(onceButton);
		expect(onConfirm).toHaveBeenCalledWith({
			stepId: 'step-test',
			approved: true,
			effect: 'allow',
			scope: 'once',
			target: 'operation',
		});
	});

	it('submits an automatic denial only once when the dialog reaches its deadline', async () => {
		vi.useFakeTimers();
		try {
			const onConfirm = vi.fn();
			render(ConfirmationDialog as any, {
				stepId: 'step-timeout',
				toolName: 'files.write',
				sessionId: 'ses-test',
				summary: '写入文件',
				permissionKey: 'files.write',
				deadlineAt: Date.now() + 1000,
				onConfirm,
			});

			await vi.advanceTimersByTimeAsync(1500);
			expect(onConfirm).toHaveBeenCalledTimes(1);

			await fireEvent.click(screen.getByRole('button', { name: '本次允许' }));
			expect(onConfirm).toHaveBeenCalledTimes(1);
		} finally {
			vi.useRealTimers();
		}
	});
});
