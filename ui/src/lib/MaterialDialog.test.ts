import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitForElementToBeRemoved } from '@testing-library/svelte';
import MaterialDialog from './MaterialDialog.svelte';
import MaterialDialogTransitionHarness from './MaterialDialogTransitionHarness.svelte';

describe('MaterialDialog', () => {
	it('keeps the overlay in the component tree for delegated events and cleanup', () => {
		const { container } = render(MaterialDialog as any, {
			open: true,
			title: '添加 Provider',
			onClose: vi.fn(),
		});

		const overlay = screen.getByRole('dialog');
		expect(overlay.parentElement).toBe(container);
	});

	it('forwards close-button clicks to onClose', async () => {
		const onClose = vi.fn();
		render(MaterialDialog as any, { open: true, title: '关闭测试', onClose });

		const dialogs = screen.getAllByRole('dialog');
		await fireEvent.click(dialogs.at(-1)!.querySelector('.md-dialog-close')!);

		expect(onClose).toHaveBeenCalledTimes(1);
	});

	it('plays its outro when a parent condition removes the dialog component', async () => {
		render(MaterialDialogTransitionHarness);
		const dialog = screen.getByRole('dialog');
		const removed = waitForElementToBeRemoved(dialog);

		await fireEvent.click(screen.getByRole('button', { name: 'Hide parent' }));

		expect(screen.getByRole('dialog')).toBe(dialog);
		await removed;
	});
});
