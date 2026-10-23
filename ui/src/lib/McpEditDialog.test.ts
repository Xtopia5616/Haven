import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import McpEditDialog from './McpEditDialog.svelte';
import type { McpServerConfigInput } from './contracts/tools.ts';

describe('McpEditDialog tool routing policy', () => {
	it('saves independent availability, catalog, and adapter settings', async () => {
		const onSave = vi.fn(async (_config: McpServerConfigInput) => {});
		const { container } = render(McpEditDialog, { onClose: vi.fn(), onSave });
		await fireEvent.input(screen.getByRole('textbox', { name: 'Name' }), {
			target: { value: 'calendar' },
		});
		await fireEvent.input(screen.getByRole('textbox', { name: 'Command' }), {
			target: { value: 'calendar-server' },
		});

		const selects = container.querySelectorAll<HTMLButtonElement>('[aria-haspopup="listbox"]');
		expect(selects).toHaveLength(4);
		await fireEvent.click(selects[3]!);
		await fireEvent.click(screen.getByRole('option', { name: 'Native and Code Mode calls' }));

		await fireEvent.input(screen.getByRole('textbox', { name: 'Per-tool overrides' }), {
			target: {
				value:
					'calendar_delete=disabled,hidden,none\ncalendar_search=enabled,hidden,codemode',
			},
		});
		expect(screen.getByText(/availability,catalog,adapters/u)).toBeTruthy();
		await fireEvent.click(screen.getByRole('button', { name: 'Add' }));

		expect(onSave).toHaveBeenCalledWith(
			expect.objectContaining({
				exposure: {
					default: {
						availability: 'enabled',
						catalog: 'deferred',
						invocation: 'native_and_codemode',
					},
					tools: {
						calendar_delete: {
							availability: 'disabled',
							catalog: 'hidden',
							invocation: 'none',
						},
						calendar_search: {
							availability: 'enabled',
							catalog: 'hidden',
							invocation: 'codemode',
						},
					},
				},
			}),
		);
	});

	it('can hide a tool from the catalog and disable its invocation separately', async () => {
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
				name: 'Disabled — no model-facing adapter may call this tool',
			}),
		);
		await fireEvent.click(selects[2]!);
		await fireEvent.click(
			screen.getByRole('option', { name: 'Hidden — omit from the general tool catalog' }),
		);
		await fireEvent.click(selects[3]!);
		await fireEvent.click(screen.getByRole('option', { name: 'Code Mode calls' }));
		await fireEvent.click(screen.getByRole('button', { name: 'Add' }));

		expect(onSave).toHaveBeenCalledWith(
			expect.objectContaining({
				exposure: {
					default: { availability: 'disabled', catalog: 'hidden', invocation: 'codemode' },
					tools: {},
				},
			}),
		);
	});
});
