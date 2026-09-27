import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import ToolsView from './ToolsView.svelte';

const { invoke, registerAppListener } = vi.hoisted(() => ({
	invoke: vi.fn(),
	registerAppListener: vi.fn(async () => ({ dispose: vi.fn() })),
}));
const { addNotification, reportError } = vi.hoisted(() => ({
	addNotification: vi.fn(),
	reportError: vi.fn(),
}));

vi.mock('$lib/tauri.ts', () => ({
	invoke,
}));

vi.mock('$lib/events.ts', () => ({
	registerAppListener,
}));

vi.mock('$lib/notificationStore.ts', async (importOriginal) => ({
	...(await importOriginal()),
	addNotification,
}));

vi.mock('$lib/errorHandling.ts', async (importOriginal) => ({
	...(await importOriginal()),
	reportError,
}));

function manifest(name: string, root: string, label: string, enabled = true) {
	return {
		identity: {
			source: 'builtin',
			catalog_group: 'system',
			root,
			operation: name.includes('.') ? name.split('.').at(-1) : null,
			stable_name: name,
		},
		model: { name, description: `${name} description`, input_schema: { type: 'object' } },
		policy: {
			risk_level: 'safe',
			permission_key: name,
			confirmation: 'none',
			idempotency: 'idempotent',
			scope: 'session',
			concurrency: 'read_only',
			effect: 'read_only',
			data_sensitivity: 'none',
			network_access: 'none',
		},
		presentation: { label, renderer: root, icon: 'tools', represented_source: 'builtin' },
		root_presentation: { label: root, description: `${root} capabilities`, icon: 'tools' },
		prompt: { when_to_use: 'use', when_not_to_use: 'never', key_operations: [name] },
		availability: {
			enabled,
			available: true,
			availability_reason: null,
			requires_connection: false,
			requires_permission: false,
		},
	};
}

describe('ToolsView toolbar actions', () => {
	beforeEach(() => {
		registerAppListener.mockClear();
		addNotification.mockClear();
		reportError.mockClear();
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
		await waitFor(() => expect(registerAppListener).toHaveBeenCalledTimes(2));
		expect(registerAppListener).toHaveBeenNthCalledWith(
			1,
			'skills:status_change',
			expect.any(Function),
			{ tag: 'tools' },
		);
		expect(registerAppListener).toHaveBeenNthCalledWith(
			2,
			'mcp:status_change',
			expect.any(Function),
			{ tag: 'tools' },
		);
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
					tools: [manifest('files', 'files', '文件'), manifest('shell', 'shell', '终端')],
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

		await waitFor(() => expect(screen.getByText('共 1 项')).toBeTruthy());
		const resourceToolbar = () => document.querySelector('.resource-toolbar');
		expect(resourceToolbar()?.querySelector('.count-chip')?.textContent).toBe('共 1 项');
		expect(document.querySelectorAll('.section .count-chip')).toHaveLength(0);

		await fireEvent.click(screen.getByRole('tab', { name: 'MCP' }));
		expect(resourceToolbar()?.querySelector('.count-chip')?.textContent).toBe('共 1 项');

		await fireEvent.click(screen.getByRole('tab', { name: '技能' }));
		expect(resourceToolbar()?.querySelector('.count-chip')?.textContent).toBe('共 2 项');
	});

	it('renders builtin tools as family, root, and operation levels', async () => {
		invoke.mockImplementation(async (command: string) => {
			if (command === 'get_tools') {
				return {
					tools: [
						manifest('files.read', 'files', '读取文件'),
						manifest('files.search', 'files', '搜索文件', false),
						manifest('shell', 'shell', '终端'),
					],
				};
			}
			if (command === 'list_mcp_tools') return [];
			if (command === 'list_skills') return [];
			return undefined;
		});

		render(ToolsView);

		await waitFor(() => expect(screen.getByText('共 1 项')).toBeTruthy());
		expect(document.querySelectorAll('[data-card-kind="builtin-family"]')).toHaveLength(1);
		expect(screen.getByText('System')).toBeTruthy();
		expect(screen.queryByText('files.read')).toBeNull();

		await fireEvent.click(screen.getByRole('button', { name: /System/ }));
		expect(document.querySelectorAll('[data-card-kind="builtin-root"]')).toHaveLength(2);
		expect(screen.getByRole('button', { name: /files/ })).toBeTruthy();
		expect(screen.queryByText('files.read')).toBeNull();

		await fireEvent.click(screen.getByRole('button', { name: /files/ }));
		expect(screen.getByText('files.read')).toBeTruthy();
		expect(screen.getByText('files.search')).toBeTruthy();
		expect(screen.queryByText('读取文件')).toBeNull();
		expect(screen.queryByText('搜索文件')).toBeNull();
		expect(screen.getByRole('switch', { name: '切换工具 files.read' })).toBeTruthy();

		await fireEvent.click(screen.getByRole('switch', { name: '切换工具 files.read' }));
		await waitFor(() =>
			expect(invoke).toHaveBeenCalledWith('set_tool_enabled', {
				name: 'files.read',
				enabled: false,
			}),
		);
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

	it('does not report a queued MCP refresh confirmation as success or failure', async () => {
		invoke.mockImplementation(async (command: string) => {
			if (command === 'get_tools') return { tools: [] };
			if (command === 'list_mcp_tools') return [];
			if (command === 'list_skills') return [];
			if (command === 'refresh_mcp_servers') {
				throw JSON.stringify({ requires_confirmation: true, confirmation_id: 'conf-test' });
			}
			return undefined;
		});

		render(ToolsView);
		await fireEvent.click(await screen.findByRole('tab', { name: 'MCP' }));
		await fireEvent.click(screen.getByRole('button', { name: '刷新' }));

		await waitFor(() =>
			expect(screen.getByRole('button', { name: '刷新' })).toHaveProperty('disabled', false),
		);
		expect(addNotification).not.toHaveBeenCalled();
		expect(reportError).not.toHaveBeenCalled();
	});

	it('does not report a queued MCP reconnect confirmation as success or failure', async () => {
		invoke.mockImplementation(async (command: string) => {
			if (command === 'get_tools') return { tools: [] };
			if (command === 'list_mcp_tools') {
				return [{ name: 'docs-server', enabled: true, status: 'Connected', tools: [] }];
			}
			if (command === 'list_skills') return [];
			if (command === 'reconnect_mcp') {
				throw JSON.stringify({ requires_confirmation: true, confirmation_id: 'conf-test' });
			}
			return undefined;
		});

		render(ToolsView);
		await fireEvent.click(await screen.findByRole('tab', { name: 'MCP' }));
		await waitFor(() => expect(screen.getByText('docs-server')).toBeTruthy());
		const refreshButtons = screen.getAllByRole('button', { name: '刷新' });
		await fireEvent.click(refreshButtons[1]);

		await waitFor(() =>
			expect(screen.getAllByRole('button', { name: '刷新' })[1]).toHaveProperty(
				'disabled',
				false,
			),
		);
		expect(addNotification).not.toHaveBeenCalled();
		expect(reportError).not.toHaveBeenCalled();
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
