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
		await fireEvent.click(screen.getByRole('tab', { name: '技能' }));
		expect(screen.getByRole('heading', { name: '技能' })).toBeTruthy();
		await fireEvent.click(screen.getByRole('tab', { name: 'MCP' }));
		expect(screen.getByRole('heading', { name: 'MCP 服务器' })).toBeTruthy();
		const addButton = screen.getByRole('button', { name: '添加' });
		const mcpToolbarButtons = Array.from(document.querySelectorAll('.toolbar-actions .md-btn'));
		const mcpToolbar = addButton.closest('.toolbar-actions');
		await fireEvent.click(screen.getByRole('tab', { name: '技能' }));
		const openFolderButton = screen.getByRole('button', { name: '打开文件夹' });
		const skillToolbarButtons = Array.from(
			document.querySelectorAll('.toolbar-actions .md-btn'),
		);
		const skillToolbar = openFolderButton.closest('.toolbar-actions');

		expect(addButton.querySelector('svg')).toBeNull();
		expect(openFolderButton.querySelector('svg')).toBeNull();
		expect(mcpToolbarButtons).toHaveLength(2);
		expect(skillToolbarButtons).toHaveLength(2);
		expect(mcpToolbar?.classList.contains('toolbar-actions--paired')).toBe(true);
		expect(skillToolbar?.classList.contains('toolbar-actions--paired')).toBe(true);
		for (const button of [...mcpToolbarButtons, ...skillToolbarButtons]) {
			expect(button.classList.contains('md-btn')).toBe(true);
		}
	});

	it('shows the active resource count in the shared filter bar', async () => {
		invoke.mockImplementation(async (command: string) => {
			if (command === 'get_tools') {
				return {
					tools: [
						{ name: 'files', description: '', risk_level: 'safe', input_schema: {} },
						{ name: 'shell', description: '', risk_level: 'high', input_schema: {} },
					],
				};
			}
			if (command === 'list_mcp_tools') {
				return [{ name: 'docs-server', enabled: true, tools: [] }];
			}
			if (command === 'list_skills') {
				return [
					{ name: 'docs', enabled: true, language: 'markdown', has_script: false },
					{ name: 'research', enabled: true, language: 'markdown', has_script: false },
				];
			}
			return undefined;
		});

		render(ToolsView);

		await waitFor(() => expect(screen.getByText('共 2 项')).toBeTruthy());
		const resourceToolbar = () => document.querySelector('.resource-toolbar');
		expect(resourceToolbar()?.querySelector('.count-chip')?.textContent).toBe('共 2 项');
		expect(document.querySelectorAll('.section .count-chip')).toHaveLength(0);

		await fireEvent.click(screen.getByRole('tab', { name: 'MCP' }));
		expect(resourceToolbar()?.querySelector('.count-chip')?.textContent).toBe('共 1 项');

		await fireEvent.click(screen.getByRole('tab', { name: '技能' }));
		expect(resourceToolbar()?.querySelector('.count-chip')?.textContent).toBe('共 2 项');
	});

	it('locks the MCP refresh action until reconciliation finishes', async () => {
		let finishRefresh: ((value: unknown) => void) | undefined;
		invoke.mockImplementation((command: string) => {
			if (command === 'get_tools') return Promise.resolve({ tools: [] });
			if (command === 'list_mcp_tools') return Promise.resolve([]);
			if (command === 'list_skills') return Promise.resolve([]);
			if (command === 'refresh_mcp_servers') {
				return new Promise((resolve) => {
					finishRefresh = resolve;
				});
			}
			return Promise.resolve(undefined);
		});

		render(ToolsView);
		await waitFor(() => expect(screen.getByRole('tab', { name: 'MCP' })).toBeTruthy());
		await fireEvent.click(screen.getByRole('tab', { name: 'MCP' }));
		const refreshButton = screen.getByRole('button', { name: '刷新' });

		await fireEvent.click(refreshButton);
		await waitFor(() => {
			expect(screen.getByRole('button', { name: '刷新中…' })).toHaveProperty(
				'disabled',
				true,
			);
		});

		finishRefresh?.({ added: [], removed: [], updated: [], failed: [] });
		await waitFor(() =>
			expect(screen.getByRole('button', { name: '刷新' })).toHaveProperty('disabled', false),
		);
	});

	it('uses the same loading contract for skill refreshes', async () => {
		let finishRefresh: (() => void) | undefined;
		invoke.mockImplementation((command: string) => {
			if (command === 'get_tools') return Promise.resolve({ tools: [] });
			if (command === 'list_mcp_tools') return Promise.resolve([]);
			if (command === 'list_skills') return Promise.resolve([]);
			if (command === 'refresh_skills') {
				return new Promise<void>((resolve) => {
					finishRefresh = resolve;
				});
			}
			return Promise.resolve(undefined);
		});

		render(ToolsView);
		await waitFor(() => expect(screen.getByRole('tab', { name: '技能' })).toBeTruthy());
		await fireEvent.click(screen.getByRole('tab', { name: '技能' }));
		await fireEvent.click(screen.getByRole('button', { name: '刷新' }));
		await waitFor(() => {
			expect(screen.getByRole('button', { name: '刷新中…' })).toHaveProperty(
				'disabled',
				true,
			);
		});

		finishRefresh?.();
		await waitFor(() =>
			expect(screen.getByRole('button', { name: '刷新' })).toHaveProperty('disabled', false),
		);
	});
});
