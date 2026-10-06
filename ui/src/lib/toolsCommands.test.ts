import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
	addMcpServer,
	listTools,
	listMcpTools,
	listSkills,
	openSkillsDir,
	reconnectMcp,
	refreshMcpServers,
	refreshSkills,
	removeMcpServer,
	resetToolCircuits,
	setSkillEnabled,
	setToolEnabled,
	toggleMcpServer,
	updateMcpServer,
} from './toolsCommands.ts';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock('$lib/tauri.ts', () => ({ invoke }));

describe('ToolsView command boundary', () => {
	beforeEach(() => {
		invoke.mockReset();
	});

	it('forwards tool manifests, empty catalog lists, and additive MCP fields unchanged', async () => {
		const manifestResponse = {
			tools: [{ identity: { stable_name: 'files.read', extension: { source: 'future' } } }],
			extension_response: true,
		};
		const mcpResponse = [
			{
				name: 'future-server',
				status: { Offline: { error: 'server unavailable' } },
				tools: [{ name: 'future-tool', input_schema: { type: 'object', extra: true } }],
				future_field: ['retained'],
			},
		];
		const emptySkills: unknown[] = [];
		invoke
			.mockResolvedValueOnce(manifestResponse)
			.mockResolvedValueOnce(mcpResponse)
			.mockResolvedValueOnce(emptySkills);

		const manifests = await listTools();
		const servers = await listMcpTools();
		const skills = await listSkills();

		expect(invoke).toHaveBeenNthCalledWith(1, 'get_tools');
		expect(invoke).toHaveBeenNthCalledWith(2, 'list_mcp_tools');
		expect(invoke).toHaveBeenNthCalledWith(3, 'list_skills');
		expect(manifests).toBe(manifestResponse);
		expect(servers).toBe(mcpResponse);
		expect(servers[0].status).toEqual({ Offline: { error: 'server unavailable' } });
		expect((servers[0] as unknown as Record<string, unknown>).future_field).toEqual(['retained']);
		expect(servers[0].tools[0].input_schema).toEqual({ type: 'object', extra: true });
		expect(skills).toBe(emptySkills);
	});

	it('rejects MCP server snapshots with a status outside the current Rust enum', async () => {
		invoke.mockResolvedValueOnce([
			{ name: 'future-server', status: { FutureStatus: { note: 'unsupported' } } },
		]);

		await expect(listMcpTools()).rejects.toThrow('Invalid MCP server snapshot status');
		expect(invoke).toHaveBeenCalledWith('list_mcp_tools');
	});

	it('keeps circuit reset as a void command and propagates its rejection', async () => {
		const failure = new Error('reset failed');
		invoke.mockRejectedValueOnce(failure);

		await expect(resetToolCircuits()).rejects.toBe(failure);
		expect(invoke).toHaveBeenCalledWith('reset_tool_circuits');
	});

	it('forwards every ToolsView MCP and Skills mutation with the Rust wire arguments', async () => {
		const config = {
			name: 'local-server',
			transport: 'stdio' as const,
			command: 'server.exe',
			args: ['--quiet'],
			env: ['TOKEN=kept-in-memory'],
			cwd: null,
			url: '',
			enabled: true,
		};
		const nameRequest = { name: 'local-server' };
		const enabledRequest = { name: 'files.read', enabled: false };
		const updateRequest = { name: 'local-server', config };
		invoke.mockResolvedValue(undefined);

		await refreshMcpServers();
		await setSkillEnabled({ name: 'example-skill', enabled: false });
		await refreshSkills();
		await openSkillsDir();
		await addMcpServer(config);
		await updateMcpServer(updateRequest);
		await removeMcpServer(nameRequest);
		await reconnectMcp(nameRequest);
		await toggleMcpServer({ name: 'local-server', enabled: true });
		await setToolEnabled(enabledRequest);

		expect(invoke.mock.calls).toEqual([
			['refresh_mcp_servers'],
			['set_skill_enabled', { name: 'example-skill', enabled: false }],
			['refresh_skills'],
			['open_skills_dir'],
			['add_mcp_server', { config }],
			['update_mcp_server', updateRequest],
			['remove_mcp_server', nameRequest],
			['reconnect_mcp', nameRequest],
			['toggle_mcp_server', { name: 'local-server', enabled: true }],
			['set_tool_enabled', enabledRequest],
		]);
		expect(invoke.mock.calls[4][1].config).toBe(config);
		expect(invoke.mock.calls[5][1]).toBe(updateRequest);
	});

	it('passes MCP refresh response extensions through and preserves invoke failures', async () => {
		const response = {
			added: ['new-server'],
			removed: [],
			updated: ['changed-server'],
			failed: ['offline-server'],
			future_field: { retained: true },
		};
		invoke.mockResolvedValueOnce(response);

		expect(await refreshMcpServers()).toBe(response);
		expect(invoke).toHaveBeenCalledWith('refresh_mcp_servers');
		invoke.mockResolvedValueOnce('C:\\Users\\test\\skills');
		expect(await openSkillsDir()).toBe('C:\\Users\\test\\skills');
		expect(invoke).toHaveBeenLastCalledWith('open_skills_dir');

		const failure = new Error('admin command rejected');
		invoke.mockReset();
		invoke.mockRejectedValueOnce(failure);
		await expect(setToolEnabled({ name: 'files.read', enabled: true })).rejects.toBe(failure);
		expect(invoke).toHaveBeenCalledWith('set_tool_enabled', {
			name: 'files.read',
			enabled: true,
		});
	});
});
