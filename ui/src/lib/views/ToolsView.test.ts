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

function deferred<T>() {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((resolvePromise) => {
		resolve = resolvePromise;
	});
	return { promise, resolve };
}

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

function mcpServer(name: string, status: string) {
	return {
		name,
		transport: 'stdio',
		command: 'server.exe',
		args: [],
		env: [],
		cwd: null,
		url: '',
		enabled: true,
		status,
		tools: [],
		last_error: null,
		diagnostic: null,
		last_seen_at: null,
	};
}

function skillInfo(name: string) {
	return {
		name,
		description: `${name} skill`,
		version: null,
		language: 'markdown',
		enabled: false,
		executable: false,
		root: `C:\\skills\\${name}`,
		has_script: false,
		unavailable_reason: null,
		manifest_error: null,
	};
}

describe('ToolsView toolbar actions', () => {
	beforeEach(() => {
		registerAppListener.mockClear();
		registerAppListener.mockImplementation(async () => ({ dispose: vi.fn() }));
		addNotification.mockClear();
		reportError.mockClear();
		invoke.mockImplementation(async (command: string) => {
			if (command === 'list_builtin_tool_manifests') return { tools: [] };
			if (command === 'list_mcp_servers') return [];
			if (command === 'list_skills') {
				return [skillInfo('docs')];
			}
			return undefined;
		});
	});

	it('stops mount setup when unmounted while the initial tool list is pending', async () => {
		const pendingManifests = deferred<{ tools: never[] }>();
		invoke.mockImplementation((command: string) => {
			if (command === 'list_builtin_tool_manifests') return pendingManifests.promise;
			return Promise.resolve(undefined);
		});

		const { unmount } = render(ToolsView);
		expect(invoke).toHaveBeenCalledTimes(1);
		unmount();
		pendingManifests.resolve({ tools: [] });
		await new Promise((resolve) => setTimeout(resolve, 0));

		expect(invoke).toHaveBeenCalledTimes(1);
		expect(registerAppListener).not.toHaveBeenCalled();
	});

	it('disposes a listener handle that resolves after unmount and stops later registrations', async () => {
		const skillsDispose = vi.fn();
		const mcpDispose = vi.fn();
		const pendingMcpListener = deferred<{ dispose: typeof mcpDispose }>();
		let registrationCount = 0;
		registerAppListener.mockImplementation(() => {
			registrationCount += 1;
			return registrationCount === 1
				? Promise.resolve({ dispose: skillsDispose })
				: pendingMcpListener.promise;
		});

		const { unmount } = render(ToolsView);
		await waitFor(() => expect(registerAppListener).toHaveBeenCalledTimes(2));
		unmount();
		expect(skillsDispose).toHaveBeenCalledTimes(1);

		pendingMcpListener.resolve({ dispose: mcpDispose });
		await waitFor(() => expect(mcpDispose).toHaveBeenCalledTimes(1));
		expect(registerAppListener).toHaveBeenCalledTimes(2);
	});

	it('does not register the MCP listener if the view is destroyed during skills listener setup', async () => {
		const skillsDispose = vi.fn();
		const pendingSkillsListener = deferred<{ dispose: typeof skillsDispose }>();
		registerAppListener.mockReturnValueOnce(pendingSkillsListener.promise);

		const { unmount } = render(ToolsView);
		await waitFor(() => expect(registerAppListener).toHaveBeenCalledTimes(1));
		unmount();
		pendingSkillsListener.resolve({ dispose: skillsDispose });

		await waitFor(() => expect(skillsDispose).toHaveBeenCalledTimes(1));
		expect(registerAppListener).toHaveBeenCalledTimes(1);
	});

	it('places MCP and skill actions beside each section heading', async () => {
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
		expect(
			(screen.getByRole('switch', { name: '切换技能 docs' }) as HTMLInputElement).disabled,
		).toBe(true);
		expect(screen.getByText('缺少入口脚本，无法执行')).toBeTruthy();
		await fireEvent.click(screen.getByRole('tab', { name: 'MCP' }));
		expect(screen.getByRole('heading', { name: 'MCP 服务器' })).toBeTruthy();
		const addButton = screen.getByRole('button', { name: '添加' });
		const mcpToolbar = addButton.closest('[role="toolbar"]');
		const mcpToolbarButtons = Array.from(mcpToolbar?.querySelectorAll('.md-btn') || []);
		await fireEvent.click(screen.getByRole('tab', { name: '技能' }));
		const openFolderButton = screen.getByRole('button', { name: '打开技能文件夹' });
		const skillToolbar = openFolderButton.closest('[role="toolbar"]');
		const skillToolbarButtons = Array.from(skillToolbar?.querySelectorAll('.md-btn') || []);

		expect(addButton.querySelector('svg')).toBeNull();
		expect(openFolderButton.querySelector('svg')).toBeNull();
		expect(mcpToolbarButtons).toHaveLength(2);
		expect(skillToolbarButtons).toHaveLength(2);
		expect(mcpToolbar?.getAttribute('aria-label')).toBe('MCP 服务器操作');
		expect(skillToolbar?.getAttribute('aria-label')).toBe('技能操作');
		for (const button of [...mcpToolbarButtons, ...skillToolbarButtons]) {
			expect(button.classList.contains('md-btn')).toBe(true);
		}
	});

	it('shows the active resource count in the shared filter bar', async () => {
		invoke.mockImplementation(async (command: string) => {
			if (command === 'list_builtin_tool_manifests') {
				return {
					tools: [manifest('files', 'files', '文件'), manifest('shell', 'shell', '终端')],
				};
			}
			if (command === 'list_mcp_servers') {
				return [mcpServer('docs-server', 'Disconnected')];
			}
			if (command === 'list_skills') {
				return [skillInfo('docs'), skillInfo('research')];
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
			if (command === 'list_builtin_tool_manifests') {
				return {
					tools: [
						manifest('files.read', 'files', '读取文件'),
						manifest('files.search', 'files', '搜索文件', false),
						manifest('shell', 'shell', '终端'),
					],
				};
			}
			if (command === 'list_mcp_servers') return [];
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
			if (command === 'list_builtin_tool_manifests') return Promise.resolve({ tools: [] });
			if (command === 'list_mcp_servers') return Promise.resolve([]);
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
			if (command === 'list_builtin_tool_manifests') return { tools: [] };
			if (command === 'list_mcp_servers') return [];
			if (command === 'list_skills') return [];
			if (command === 'refresh_mcp_servers') {
				throw JSON.stringify({ requires_confirmation: true, confirmation_id: 'conf-b7a65e97699c7c548225828bfc5e17f9' });
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
			if (command === 'list_builtin_tool_manifests') return { tools: [] };
			if (command === 'list_mcp_servers') {
				return [mcpServer('docs-server', 'Connected')];
			}
			if (command === 'list_skills') return [];
			if (command === 'reconnect_mcp_server') {
				throw JSON.stringify({ requires_confirmation: true, confirmation_id: 'conf-b7a65e97699c7c548225828bfc5e17f9' });
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
			if (command === 'list_builtin_tool_manifests') return Promise.resolve({ tools: [] });
			if (command === 'list_mcp_servers') return Promise.resolve([]);
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
