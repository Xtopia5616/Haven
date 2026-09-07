import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import MaterialDialog from './MaterialDialog.svelte';

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
});
