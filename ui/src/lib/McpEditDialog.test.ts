import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import McpEditDialog from './McpEditDialog.svelte';
import type { McpServerConfigInput } from './contracts/tools.ts';

describe('McpEditDialog CodeMode exposure', () => {
	it('saves a Legacy/CodeMode default and per-tool overrides', async () => {
		const onSave = vi.fn(async (_config: McpServerConfigInput) => {});
		const { container } = render(McpEditDialog, { onClose: vi.fn(), onSave });
		await fireEvent.input(screen.getByRole('textbox', { name: 'Name' }), {
			target: { value: 'calendar' },
		});
		await fireEvent.input(screen.getByRole('textbox', { name: 'Command' }), {
			target: { value: 'calendar-server' },
		});

		const selects = container.querySelectorAll<HTMLButtonElement>('[aria-haspopup="listbox"]');
		expect(selects).toHaveLength(2);
		await fireEvent.click(selects[1]!);
		await fireEvent.click(
			screen.getByRole('option', {
				name: 'CodeMode — compose allowed tools in a bounded script',
			}),
		);

		await fireEvent.input(screen.getByRole('textbox', { name: 'Per-tool overrides' }), {
			target: { value: 'calendar_delete=hidden\ncalendar_search=codemode' },
		});
		expect(screen.getByText(/Calls requiring confirmation must use Legacy/u)).toBeTruthy();
		await fireEvent.click(screen.getByRole('button', { name: 'Add' }));

		expect(onSave).toHaveBeenCalledWith(
			expect.objectContaining({
				exposure: {
					default: 'codemode',
					tools: { calendar_delete: 'hidden', calendar_search: 'codemode' },
				},
			}),
		);
	});

	it('persists Model-only exposure as a top-level-only policy', async () => {
		const onSave = vi.fn(async (_config: McpServerConfigInput) => {});
		const { container } = render(McpEditDialog, { onClose: vi.fn(), onSave });
		await fireEvent.input(screen.getByRole('textbox', { name: 'Name' }), {
			target: { value: 'calendar' },
		});
		await fireEvent.input(screen.getByRole('textbox', { name: 'Command' }), {
			target: { value: 'calendar-server' },
		});

		const selects = container.querySelectorAll<HTMLButtonElement>('[aria-haspopup="listbox"]');
		await fireEvent.click(selects[1]!);
		await fireEvent.click(
			screen.getByRole('option', {
				name: 'Model-only — direct model calls only; no nested Workflow or CodeMode',
			}),
		);
		await fireEvent.click(screen.getByRole('button', { name: 'Add' }));

		expect(onSave).toHaveBeenCalledWith(
			expect.objectContaining({
				exposure: { default: 'model_only', tools: {} },
			}),
		);
	});
});
