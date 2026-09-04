import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import ToolsView from './ToolsView.svelte';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock('$lib/tauri.ts', () => ({
	invoke,
}));

vi.mock('$lib/events.ts', () => ({
	registerOne: vi.fn(async () => ({ dispose: vi.fn() })),
}));

describe('ToolsView toolbar actions', () => {
	beforeEach(() => {
		invoke.mockImplementation(async (command: string) => {
			if (command === 'get_tools') return { tools: [] };
			if (command === 'list_mcp_tools') return [];
			if (command === 'list_skills') {
				return [{ name: 'docs', enabled: true, language: 'markdown', has_script: false }];
			}
			return undefined;
		});
	});

	it('keeps MCP and skill toolbar actions text-only', async () => {
		render(ToolsView);

		await waitFor(() => expect(screen.getByRole('tab', { name: '技能' })).toBeTruthy());
		await fireEvent.click(screen.getByRole('tab', { name: 'MCP' }));
		const addButton = screen.getByRole('button', { name: '添加' });
		await fireEvent.click(screen.getByRole('tab', { name: '技能' }));
		const openFolderButton = screen.getByRole('button', { name: '打开文件夹' });

		expect(addButton.querySelector('svg')).toBeNull();
		expect(openFolderButton.querySelector('svg')).toBeNull();
	});
});
