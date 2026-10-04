import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
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
		expect(
			screen.getByRole('button', { name: '更多拒绝选项' }).getAttribute('data-variant'),
		).toBe('danger');

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

		await fireEvent.click(screen.getByRole('button', { name: '更多拒绝选项' }));
		expect(
			Array.from(container.querySelectorAll('.deny-menu .menu-item')).every((item) =>
				item.classList.contains('danger'),
			),
		).toBe(true);

		await fireEvent.click(onceButton);
		expect(onConfirm).toHaveBeenCalledWith({
			stepId: 'step-test',
			approved: true,
			effect: 'allow',
			scope: 'once',
			target: 'operation',
			timedOut: false,
		});
	});

	it('closes without deciding and can be shown again while the request stays pending', async () => {
		const onDismiss = vi.fn();
		const onConfirm = vi.fn();
		const { rerender } = render(ConfirmationDialog as any, {
			open: true,
			stepId: 'step-close',
			toolName: 'files.write',
			onDismiss,
			onConfirm,
		});

		await fireEvent.click(screen.getByRole('button', { name: '关闭权限确认' }));
		expect(onDismiss).toHaveBeenCalledWith('step-close');
		expect(onConfirm).not.toHaveBeenCalled();

		await rerender({
			open: false,
			stepId: 'step-close',
			toolName: 'files.write',
			onDismiss,
			onConfirm,
		});
		await waitFor(() => expect(screen.queryByRole('button', { name: '本次允许' })).toBeNull());

		await rerender({
			open: true,
			stepId: 'step-close',
			toolName: 'files.write',
			onDismiss,
			onConfirm,
		});
		await waitFor(() => expect(screen.getByRole('button', { name: '本次允许' })).toBeTruthy());
		expect(onConfirm).not.toHaveBeenCalled();
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

	it('does not offer session grants to confirmations without a persisted session', async () => {
		render(ConfirmationDialog as any, {
			stepId: 'conf-ui-only',
			toolName: 'mcp__server__write',
			permissionKey: 'mcp.server.write',
			onConfirm: vi.fn(),
		});

		expect(screen.queryByRole('button', { name: '本对话允许此操作' })).toBeNull();
		await fireEvent.click(screen.getByRole('button', { name: '更多允许' }));
		expect(screen.queryByText('本对话')).toBeNull();
		expect(screen.getByRole('menuitem', { name: '永久允许此操作' })).toBeTruthy();
		await fireEvent.click(screen.getByRole('button', { name: '更多拒绝选项' }));
		expect(screen.queryByRole('menuitem', { name: '本对话拒绝此操作' })).toBeNull();
	});

	it('keeps a rejected pre-execution confirmation submission retryable', async () => {
		const onConfirm = vi.fn().mockResolvedValueOnce(false).mockResolvedValueOnce(true);
		render(ConfirmationDialog as any, {
			stepId: 'conf-retry',
			toolName: 'mcp__server__write',
			onConfirm,
		});

		await fireEvent.click(screen.getByRole('button', { name: '本次允许' }));
		await fireEvent.click(screen.getByRole('button', { name: '本次允许' }));
		expect(onConfirm).toHaveBeenCalledTimes(2);
	});
});
