import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import MaterialDialog from './MaterialDialog.svelte';

describe('MaterialDialog', () => {
	it('mounts the overlay on the document body for viewport centering', () => {
		render(MaterialDialog as any, {
			open: true,
			title: '添加 Provider',
			onClose: vi.fn(),
		});

		const overlay = screen.getByRole('dialog');
		expect(overlay.parentElement).toBe(document.body);
	});
});
