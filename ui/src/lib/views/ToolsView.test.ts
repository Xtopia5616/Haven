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
	let reject!: (reason?: unknown) => void;
	const promise = new Promise<T>((resolvePromise, rejectPromise) => {
		resolve = resolvePromise;
		reject = rejectPromise;
	});
	return { promise, resolve, reject };
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

function manifest(
	name: string,
	root: string,
	label: string,
	enabled = true,
	catalogGroup = 'system',
) {
	return {
		identity: {
			source: 'builtin',
			catalog_group: catalogGroup,
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
		target_binding: null,
		supports_resources: false,
		supports_resource_subscriptions: false,
		supports_prompts: false,
		exposure: { default: 'deferred', tools: {} },
		status,
		tools: [],
		last_error: null,
		diagnostic: null,
		last_seen_at: null,
	};
}

function skillInfo(name: string, enabled = false, hasScript = false) {
	return {
		name,
		description: `${name} skill`,
		version: null,
		language: 'markdown',
		enabled,
		executable: enabled && hasScript,
		root: `C:\\skills\\${name}`,
		has_script: hasScript,
		unavailable_reason: null,
		manifest_error: null,
	};
}

describe('ToolsView toolbar actions', () => {
	beforeEach(() => {
		invoke.mockClear();
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

	it('keeps builtin load failures visible and recovers through the in-page retry', async () => {
		let builtinAttempts = 0;
		invoke.mockImplementation(async (command: string) => {
			if (command === 'list_builtin_tool_manifests') {
				builtinAttempts += 1;
				if (builtinAttempts === 1) throw new Error('temporary failure');
				return { tools: [manifest('system.test', 'system', '测试工具')] };
			}
			if (command === 'list_mcp_servers' || command === 'list_skills') return [];
			return undefined;
		});

		render(ToolsView);
		await screen.findByRole('heading', { name: '内置工具加载失败' });
		await fireEvent.click(screen.getByRole('button', { name: '重试' }));

		expect(await screen.findByText('系统与设备')).toBeTruthy();
		expect(screen.queryByRole('heading', { name: '内置工具加载失败' })).toBeNull();
		expect(builtinAttempts).toBe(2);
		expect(addNotification).not.toHaveBeenCalled();
	});

	it('shows MCP and skill load errors separately from unconfigured empty states', async () => {
		invoke.mockImplementation(async (command: string) => {
			if (command === 'list_builtin_tool_manifests') return { tools: [] };
			if (command === 'list_mcp_servers' || command === 'list_skills') {
				throw new Error('temporary failure');
			}
			return undefined;
		});

		render(ToolsView);
		await fireEvent.click(await screen.findByRole('tab', { name: 'MCP' }));
		await screen.findByRole('heading', { name: 'MCP 服务器加载失败' });
		expect(screen.queryByRole('heading', { name: '尚未配置 MCP 服务器' })).toBeNull();

		await fireEvent.click(screen.getByRole('tab', { name: '技能' }));
		await screen.findByRole('heading', { name: '技能列表加载失败' });
		expect(screen.queryByRole('heading', { name: '暂无技能' })).toBeNull();
		expect(addNotification).not.toHaveBeenCalled();
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

	it('runs a Skill through the regular management authorization action', async () => {
		invoke.mockImplementation(async (command: string) => {
			if (command === 'list_builtin_tool_manifests') return { tools: [] };
			if (command === 'list_mcp_servers') return [];
			if (command === 'list_skills') return [skillInfo('docs', true, true)];
			if (command === 'execute_skill') {
				return { success: true, output: { ok: true }, error: null };
			}
			return undefined;
		});

		render(ToolsView);
		await fireEvent.click(await screen.findByRole('tab', { name: '技能' }));
		await screen.findByText('docs');
		await fireEvent.click(screen.getByText('docs').closest('[role="button"]')!);
		await fireEvent.click(screen.getByRole('button', { name: '运行' }));

		await waitFor(() =>
			expect(invoke).toHaveBeenCalledWith('execute_skill', {
				name: 'docs',
				params: {},
			}),
		);
	});

	it('serializes skill toggles and reconciles the latest failed intent from the backend', async () => {
		const skillWrites = [deferred<void>(), deferred<void>()];
		let writeIndex = 0;
		invoke.mockImplementation((command: string) => {
			if (command === 'list_builtin_tool_manifests') return Promise.resolve({ tools: [] });
			if (command === 'list_mcp_servers') return Promise.resolve([]);
			if (command === 'list_skills') return Promise.resolve([skillInfo('docs', true, true)]);
			if (command === 'set_skill_enabled') return skillWrites[writeIndex++].promise;
			return Promise.resolve(undefined);
		});

		render(ToolsView);
		await fireEvent.click(await screen.findByRole('tab', { name: '技能' }));
		const getSkillSwitch = () =>
			screen.getByRole('switch', { name: '切换技能 docs' }) as HTMLInputElement;
		await waitFor(() => expect(getSkillSwitch().checked).toBe(true));

		await fireEvent.click(getSkillSwitch());
		await waitFor(() =>
			expect(invoke).toHaveBeenCalledWith('set_skill_enabled', {
				name: 'docs',
				enabled: false,
			}),
		);
		await fireEvent.click(getSkillSwitch());
		expect(getSkillSwitch().checked).toBe(true);
		expect(
			invoke.mock.calls.filter(([command]) => command === 'set_skill_enabled'),
		).toHaveLength(1);

		skillWrites[0].reject(new Error('disable failed'));
		await waitFor(() =>
			expect(
				invoke.mock.calls.filter(([command]) => command === 'set_skill_enabled'),
			).toHaveLength(2),
		);
		skillWrites[1].reject(new Error('enable failed'));

		await waitFor(() => {
			expect(invoke.mock.calls.filter(([command]) => command === 'list_skills')).toHaveLength(
				2,
			);
			expect(getSkillSwitch().checked).toBe(true);
		});
		expect(reportError).toHaveBeenCalledTimes(1);
		expect(reportError).toHaveBeenCalledWith(new Error('enable failed'), {
			context: 'ToolsView',
			message: '切换 docs 失败',
			log: false,
		});
	});

	it('serializes MCP toggles and preserves the latest intent across two failures', async () => {
		const mcpWrites = [deferred<void>(), deferred<void>()];
		let writeIndex = 0;
		invoke.mockImplementation((command: string) => {
			if (command === 'list_builtin_tool_manifests') return Promise.resolve({ tools: [] });
			if (command === 'list_mcp_servers') {
				return Promise.resolve([mcpServer('docs-server', 'Connected')]);
			}
			if (command === 'list_skills') return Promise.resolve([]);
			if (command === 'toggle_mcp_server') return mcpWrites[writeIndex++].promise;
			return Promise.resolve(undefined);
		});

		render(ToolsView);
		await fireEvent.click(await screen.findByRole('tab', { name: 'MCP' }));
		const getMcpSwitch = () =>
			screen.getByRole('switch', { name: '切换 docs-server' }) as HTMLInputElement;
		await waitFor(() => expect(getMcpSwitch().checked).toBe(true));

		await fireEvent.click(getMcpSwitch());
		await waitFor(() =>
			expect(invoke).toHaveBeenCalledWith('toggle_mcp_server', {
				name: 'docs-server',
				enabled: false,
			}),
		);
		await fireEvent.click(getMcpSwitch());
		expect(getMcpSwitch().checked).toBe(true);
		expect(
			invoke.mock.calls.filter(([command]) => command === 'toggle_mcp_server'),
		).toHaveLength(1);

		mcpWrites[0].reject(new Error('disable failed'));
		await waitFor(() =>
			expect(
				invoke.mock.calls.filter(([command]) => command === 'toggle_mcp_server'),
			).toHaveLength(2),
		);
		mcpWrites[1].reject(new Error('enable failed'));

		await waitFor(() => {
			expect(
				invoke.mock.calls.filter(([command]) => command === 'list_mcp_servers'),
			).toHaveLength(2);
			expect(getMcpSwitch().checked).toBe(true);
		});
		expect(reportError).toHaveBeenCalledTimes(1);
		expect(reportError).toHaveBeenCalledWith(new Error('enable failed'), {
			context: 'ToolsView',
			message: '切换 docs-server 失败',
			log: false,
		});
	});

	it('shows the active resource count in the shared filter bar', async () => {
		invoke.mockImplementation(async (command: string) => {
			if (command === 'list_builtin_tool_manifests') {
				return {
					tools: [
						manifest('files', 'files', '文件', true, 'files'),
						manifest('shell', 'shell', '终端', true, 'shell'),
					],
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

		await waitFor(() => expect(screen.getByText('共 2 个操作')).toBeTruthy());
		const resourceToolbar = () => document.querySelector('.resource-toolbar');
		expect(resourceToolbar()?.querySelector('.count-chip')?.textContent).toBe('共 2 个操作');
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
						manifest('files.read', 'files', '读取文件', true, 'files'),
						manifest('files.search', 'files', '搜索文件', false, 'files'),
						manifest('shell', 'shell', '终端', true, 'shell'),
					],
				};
			}
			if (command === 'list_mcp_servers') return [];
			if (command === 'list_skills') return [];
			return undefined;
		});

		render(ToolsView);

		await waitFor(() => expect(screen.getByText('共 3 个操作')).toBeTruthy());
		expect(document.querySelectorAll('[data-card-kind="builtin-family"]')).toHaveLength(2);
		expect(screen.getByText('文件系统')).toBeTruthy();
		expect(screen.getByText('命令行')).toBeTruthy();
		expect(screen.queryByText('files.read')).toBeNull();

		await fireEvent.click(screen.getByRole('button', { name: /文件系统/ }));
		expect(document.querySelectorAll('[data-card-kind="builtin-root"]')).toHaveLength(1);
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
				throw JSON.stringify({
					requires_confirmation: true,
					confirmation_id: 'conf-b7a65e97699c7c548225828bfc5e17f9',
				});
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
				throw JSON.stringify({
					requires_confirmation: true,
					confirmation_id: 'conf-b7a65e97699c7c548225828bfc5e17f9',
				});
			}
			return undefined;
		});

		render(ToolsView);
		await fireEvent.click(await screen.findByRole('tab', { name: 'MCP' }));
		await waitFor(() => expect(screen.getByText('docs-server')).toBeTruthy());
		const reconnectButton = screen.getByRole('button', {
			name: '重新连接 MCP 服务器 docs-server',
		});
		await fireEvent.click(reconnectButton);

		await waitFor(() =>
			expect(reconnectButton).toHaveProperty('disabled', false),
		);
		expect(addNotification).not.toHaveBeenCalled();
		expect(reportError).not.toHaveBeenCalled();
	});

	it('marks an MCP refresh with any failed server as a warning, including mixed results', async () => {
		invoke.mockImplementation(async (command: string) => {
			if (command === 'list_builtin_tool_manifests') return { tools: [] };
			if (command === 'list_mcp_servers') return [];
			if (command === 'list_skills') return [];
			if (command === 'refresh_mcp_servers') {
				return {
					added: ['new-server'],
					removed: [],
					updated: [],
					failed: ['offline-server'],
				};
			}
			return undefined;
		});

		render(ToolsView);
		await fireEvent.click(await screen.findByRole('tab', { name: 'MCP' }));
		await fireEvent.click(screen.getByRole('button', { name: '刷新' }));

		await waitFor(() =>
			expect(addNotification).toHaveBeenCalledWith(
				'MCP 刷新: 新增 new-server；offline-server 连接失败',
				'warning',
				3000,
			),
		);
	});

	it('reports the MCP server name and add operation when adding fails', async () => {
		invoke.mockImplementation(async (command: string) => {
			if (command === 'list_builtin_tool_manifests') return { tools: [] };
			if (command === 'list_mcp_servers') return [];
			if (command === 'list_skills') return [];
			if (command === 'add_mcp_server') throw new Error('add failed');
			return undefined;
		});

		render(ToolsView);
		await fireEvent.click(await screen.findByRole('tab', { name: 'MCP' }));
		await fireEvent.click(screen.getByRole('button', { name: '添加' }));
		await fireEvent.input(screen.getByLabelText('Name'), { target: { value: 'new-server' } });
		await fireEvent.input(screen.getByLabelText('Command'), {
			target: { value: 'mcp-server' },
		});
		await fireEvent.click(screen.getByRole('button', { name: 'Add' }));

		await waitFor(() =>
			expect(reportError).toHaveBeenCalledWith(new Error('add failed'), {
				context: 'ToolsView',
				message: '添加 MCP 服务器 new-server 失败',
				log: false,
			}),
		);
	});

	it('reports the MCP server name and update operation when updating fails', async () => {
		invoke.mockImplementation(async (command: string) => {
			if (command === 'list_builtin_tool_manifests') return { tools: [] };
			if (command === 'list_mcp_servers') return [mcpServer('docs-server', 'Connected')];
			if (command === 'list_skills') return [];
			if (command === 'update_mcp_server') throw new Error('update failed');
			return undefined;
		});

		render(ToolsView);
		await fireEvent.click(await screen.findByRole('tab', { name: 'MCP' }));
		await waitFor(() => expect(screen.getByText('docs-server')).toBeTruthy());
		await fireEvent.click(
			screen.getByRole('button', { name: '编辑 MCP 服务器 docs-server' }),
		);
		await fireEvent.click(screen.getByRole('button', { name: 'Update' }));

		await waitFor(() =>
			expect(reportError).toHaveBeenCalledWith(new Error('update failed'), {
				context: 'ToolsView',
				message: '更新 MCP 服务器 docs-server 失败',
				log: false,
			}),
		);
	});

	it('reports the MCP server name and remove operation when removal fails', async () => {
		invoke.mockImplementation(async (command: string) => {
			if (command === 'list_builtin_tool_manifests') return { tools: [] };
			if (command === 'list_mcp_servers') return [mcpServer('docs-server', 'Connected')];
			if (command === 'list_skills') return [];
			if (command === 'remove_mcp_server') throw new Error('remove failed');
			return undefined;
		});

		render(ToolsView);
		await fireEvent.click(await screen.findByRole('tab', { name: 'MCP' }));
		await waitFor(() => expect(screen.getByText('docs-server')).toBeTruthy());
		await fireEvent.click(
			screen.getByRole('button', { name: '移除 MCP 服务器 docs-server' }),
		);

		await waitFor(() =>
			expect(reportError).toHaveBeenCalledWith(new Error('remove failed'), {
				context: 'ToolsView',
				message: '移除 MCP 服务器 docs-server 失败',
				log: false,
			}),
		);
	});

	it('reports the MCP server name and reconnect operation when reconnecting fails', async () => {
		invoke.mockImplementation(async (command: string) => {
			if (command === 'list_builtin_tool_manifests') return { tools: [] };
			if (command === 'list_mcp_servers') return [mcpServer('docs-server', 'Connected')];
			if (command === 'list_skills') return [];
			if (command === 'reconnect_mcp_server') throw new Error('reconnect failed');
			return undefined;
		});

		render(ToolsView);
		await fireEvent.click(await screen.findByRole('tab', { name: 'MCP' }));
		await waitFor(() => expect(screen.getByText('docs-server')).toBeTruthy());
		await fireEvent.click(
			screen.getByRole('button', { name: '重新连接 MCP 服务器 docs-server' }),
		);

		await waitFor(() =>
			expect(reportError).toHaveBeenCalledWith(new Error('reconnect failed'), {
				context: 'ToolsView',
				message: '刷新 MCP 服务器 docs-server 连接失败',
				log: false,
			}),
		);
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
