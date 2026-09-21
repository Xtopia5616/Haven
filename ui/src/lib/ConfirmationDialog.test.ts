import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import ConfirmationDialog from './ConfirmationDialog.svelte';

describe('ConfirmationDialog', () => {
	it('emphasizes one-time approval and de-emphasizes persistent approval', async () => {
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
		const persistentButton = screen.getByRole('button', { name: '永久允许此操作' });
		expect(onceButton.classList.contains('md-btn--filled')).toBe(true);
		expect(persistentButton.classList.contains('md-btn--text')).toBe(true);

		const allowButtonLabels = Array.from(container.querySelectorAll('.allow-group button')).map(
			(button) => button.textContent?.trim() || button.getAttribute('aria-label'),
		);
		expect(allowButtonLabels).toEqual([
			'永久允许此操作',
			'更多允许范围和期限',
			'本对话允许此操作',
			'本次允许',
		]);

		await fireEvent.click(onceButton);
		expect(onConfirm).toHaveBeenCalledWith({
			stepId: 'step-test',
			approved: true,
			effect: 'allow',
			scope: 'once',
			target: 'operation',
		});
	});
});
