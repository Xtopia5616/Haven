import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
	addMcpServer,
	executeSkill,
	listBuiltinToolManifests,
	listMcpPrompts,
	listMcpResources,
	listMcpResourceTemplates,
	listMcpServers,
	listSkills,
	openSkillsDir,
	getMcpPrompt,
	readMcpResource,
	respondMcpFeatureInput,
	cancelMcpFeatureInput,
	subscribeMcpResource,
	unsubscribeMcpResource,
	reconnectMcpServer,
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

function makeMcpServerSnapshot(overrides: Record<string, unknown> = {}) {
	return {
		name: 'future-server',
		transport: 'stdio',
		command: 'server.exe',
		args: ['--quiet'],
		env: ['TOKEN=<redacted>'],
		cwd: null,
		url: '',
		enabled: true,
		target_binding: 'mcp-target:0123456789abcdef:fedcba9876543210',
		supports_resources: false,
		supports_resource_subscriptions: false,
		supports_prompts: false,
		exposure: {
			default: { availability: 'enabled', catalog: 'deferred', invocation: 'native' },
			tools: {},
		},
		status: 'Disconnected',
		tools: [
			{
				name: 'future-tool',
				description: 'A future MCP tool',
				input_schema: { type: 'object', extra: true },
			},
		],
		last_error: null,
		diagnostic: null,
		last_seen_at: 12345,
		...overrides,
	};
}

function makeSkillInfo(overrides: Record<string, unknown> = {}) {
	return {
		name: 'meeting-notes',
		description: 'Capture meeting notes',
		version: '1.0.0',
		language: 'python',
		enabled: true,
		executable: true,
		has_script: true,
		unavailable_reason: null,
		manifest_error: null,
		...overrides,
	};
}

function completeFeatureOutcome(payload: { resourceRead?: unknown; prompt?: unknown }) {
	return {
		status: 'complete',
		resourceRead: payload.resourceRead ?? null,
		prompt: payload.prompt ?? null,
		requestId: null,
		round: null,
		requests: [],
	};
}

describe('ToolsView command boundary', () => {
	beforeEach(() => {
		invoke.mockReset();
	});

	it('maps MCP snapshots to camelCase views while preserving dynamic schemas', async () => {
		const manifestResponse = {
			tools: [{ identity: { stable_name: 'files.read', extension: { source: 'future' } } }],
			extension_response: true,
		};
		const mcpResponse = [
			makeMcpServerSnapshot({
				status: { Offline: { error: 'server unavailable' } },
				last_error: 'connection refused',
				diagnostic: 'server did not respond',
				future_field: ['ignored'],
			}),
		];
		const emptySkills: unknown[] = [];
		invoke
			.mockResolvedValueOnce(manifestResponse)
			.mockResolvedValueOnce(mcpResponse)
			.mockResolvedValueOnce(emptySkills);

		const manifests = await listBuiltinToolManifests();
		const servers = await listMcpServers();
		const skills = await listSkills();

		expect(invoke).toHaveBeenNthCalledWith(1, 'list_builtin_tool_manifests');
		expect(invoke).toHaveBeenNthCalledWith(2, 'list_mcp_servers');
		expect(invoke).toHaveBeenNthCalledWith(3, 'list_skills');
		expect(manifests).toEqual(manifestResponse);
		expect(servers).toEqual([
			{
				name: 'future-server',
				transport: 'stdio',
				command: 'server.exe',
				args: ['--quiet'],
				env: ['TOKEN=<redacted>'],
				cwd: null,
				url: '',
				enabled: true,
				targetBinding: 'mcp-target:0123456789abcdef:fedcba9876543210',
				supportsResources: false,
				supportsResourceSubscriptions: false,
				supportsPrompts: false,
				exposure: {
					default: { availability: 'enabled', catalog: 'deferred', invocation: 'native' },
					tools: {},
				},
				status: { Offline: { error: 'server unavailable' } },
				tools: [
					{
						name: 'future-tool',
						description: 'A future MCP tool',
						inputSchema: { type: 'object', extra: true },
					},
				],
				lastError: 'connection refused',
				diagnostic: 'server did not respond',
				lastSeenAt: 12345,
			},
		]);
		expect(servers[0]).not.toHaveProperty('last_error');
		expect(skills).toEqual(emptySkills);
	});

	it('forwards user-selected MCP resource and prompt requests with the live target binding', async () => {
		const target = {
			name: 'future-server',
			targetBinding: 'mcp-target:0123456789abcdef:fedcba9876543210',
		};
		invoke
			.mockResolvedValueOnce({ resources: [], ttlMs: null, cacheScope: 'private' })
			.mockResolvedValueOnce({ resourceTemplates: [], ttlMs: null, cacheScope: 'private' })
			.mockResolvedValueOnce(
				completeFeatureOutcome({
					resourceRead: { contents: [], ttlMs: null, cacheScope: 'private' },
				}),
			)
			.mockResolvedValueOnce(true)
			.mockResolvedValueOnce(true)
			.mockResolvedValueOnce({ prompts: [], ttlMs: null, cacheScope: 'private' })
			.mockResolvedValueOnce(
				completeFeatureOutcome({
					prompt: {
						description: null,
						messages: [{ role: 'user', content: { type: 'text', text: 'Hello' } }],
						ttlMs: null,
						cacheScope: 'private',
					},
				}),
			)
			.mockResolvedValueOnce(
				completeFeatureOutcome({
					prompt: {
						description: null,
						messages: [{ role: 'user', content: { type: 'text', text: 'Done' } }],
						ttlMs: null,
						cacheScope: 'private',
					},
				}),
			)
			.mockResolvedValueOnce(true);

		expect(await listMcpResources({ ...target, forceRefresh: false })).toMatchObject({
			resources: [],
		});
		expect(await listMcpResourceTemplates({ ...target, forceRefresh: false })).toMatchObject({
			resourceTemplates: [],
		});
		expect(
			await readMcpResource({ ...target, uri: 'mcp://selected/doc', forceRefresh: false }),
		).toMatchObject({ status: 'complete', resourceRead: { contents: [] } });
		expect(await subscribeMcpResource({ ...target, uri: 'mcp://selected/doc' })).toBe(true);
		expect(await unsubscribeMcpResource({ ...target, uri: 'mcp://selected/doc' })).toBe(true);
		expect(await listMcpPrompts({ ...target, forceRefresh: false })).toMatchObject({
			prompts: [],
		});
		expect(
			await getMcpPrompt({
				...target,
				promptName: 'review',
				arguments: { topic: 'selected by user' },
			}),
		).toMatchObject({
			status: 'complete',
			prompt: { description: null, messages: [{ role: 'user' }] },
		});
		const answerRequest = {
			...target,
			requestId: 5,
			answers: { ask: { action: 'accept' as const, content: { topic: 'MCP' } } },
		};
		expect(await respondMcpFeatureInput(answerRequest)).toMatchObject({ status: 'complete' });
		const cancelRequest = { ...target, requestId: 5 };
		expect(await cancelMcpFeatureInput(cancelRequest)).toBe(true);
		expect(invoke.mock.calls).toEqual([
			['list_mcp_resources', { ...target, forceRefresh: false }],
			['list_mcp_resource_templates', { ...target, forceRefresh: false }],
			['read_mcp_resource', { ...target, uri: 'mcp://selected/doc', forceRefresh: false }],
			['subscribe_mcp_resource', { ...target, uri: 'mcp://selected/doc' }],
			['unsubscribe_mcp_resource', { ...target, uri: 'mcp://selected/doc' }],
			['list_mcp_prompts', { ...target, forceRefresh: false }],
			[
				'get_mcp_prompt',
				{ ...target, promptName: 'review', arguments: { topic: 'selected by user' } },
			],
			['respond_mcp_feature_input', answerRequest],
			['cancel_mcp_feature_input', cancelRequest],
		]);
	});

	it.each([null, {}, { tools: null }, { tools: {} }, { tools: 'invalid' }])(
		'rejects an invalid builtin manifest envelope %j',
		async (response) => {
			invoke.mockResolvedValueOnce(response);
			await expect(listBuiltinToolManifests()).rejects.toThrow(
				'Invalid builtin tool manifest list response',
			);
		},
	);

	it('maps Skills list responses to the renderer view', async () => {
		invoke.mockResolvedValueOnce([makeSkillInfo()]);

		expect(await listSkills()).toEqual([
			{
				name: 'meeting-notes',
				description: 'Capture meeting notes',
				version: '1.0.0',
				language: 'python',
				enabled: true,
				executable: true,
				hasScript: true,
				unavailableReason: null,
				manifestError: null,
			},
		]);
		expect(invoke).toHaveBeenCalledWith('list_skills');
	});

	it('rejects MCP server snapshots with a status outside the current Rust enum', async () => {
		invoke.mockResolvedValueOnce([
			makeMcpServerSnapshot({ status: { FutureStatus: { note: 'unsupported' } } }),
		]);

		await expect(listMcpServers()).rejects.toThrow('Invalid MCP server snapshot');
		expect(invoke).toHaveBeenCalledWith('list_mcp_servers');
	});

	it('rejects MCP server snapshots with an unknown transport', async () => {
		invoke.mockResolvedValueOnce([makeMcpServerSnapshot({ transport: 'future-transport' })]);

		await expect(listMcpServers()).rejects.toThrow('Invalid MCP server snapshot');
	});

	it.each([
		['profile name', { name: 'invalid profile name' }],
		['command', { command: null }],
		['arguments', { args: ['--ok', 3] }],
		['environment', { env: [null] }],
		['working directory', { cwd: 1 }],
		['URL', { url: false }],
		['enabled state', { enabled: 'true' }],
		['exposure mode', { exposure: { default: 'future', tools: {} } }],
		[
			'exposure overrides',
			{
				exposure: {
					default: { availability: 'enabled', catalog: 'deferred', invocation: 'native' },
					tools: {
						'': { availability: 'disabled', catalog: 'hidden', invocation: 'none' },
					},
				},
			},
		],
		['tool schema', { tools: [{ name: 'tool', description: 'Tool', input_schema: null }] }],
		['last error', { last_error: 1 }],
		['diagnostic', { diagnostic: false }],
		['last-seen timestamp', { last_seen_at: 1.5 }],
	])('rejects MCP snapshots with invalid %s data', async (_field, override) => {
		invoke.mockResolvedValueOnce([makeMcpServerSnapshot(override)]);

		await expect(listMcpServers()).rejects.toThrow('Invalid MCP server snapshot');
	});

	it('rejects duplicate MCP profile identities in one list response', async () => {
		invoke.mockResolvedValueOnce([makeMcpServerSnapshot(), makeMcpServerSnapshot()]);

		await expect(listMcpServers()).rejects.toThrow('Duplicate MCP server name');
	});

	it('rejects duplicate tool identities within an MCP server snapshot', async () => {
		invoke.mockResolvedValueOnce([
			makeMcpServerSnapshot({
				tools: [
					{ name: 'duplicate', description: 'First', input_schema: {} },
					{ name: 'duplicate', description: 'Second', input_schema: {} },
				],
			}),
		]);

		await expect(listMcpServers()).rejects.toThrow('Invalid MCP server snapshot');
	});

	it.each(['', '   '])('rejects blank MCP tool names (%j)', async (name) => {
		invoke.mockResolvedValueOnce([
			makeMcpServerSnapshot({
				tools: [{ name, description: 'Blank name', input_schema: {} }],
			}),
		]);

		await expect(listMcpServers()).rejects.toThrow('Invalid MCP server snapshot');
	});

	it('accepts an empty MCP list and null optional snapshot fields', async () => {
		invoke.mockResolvedValueOnce([]);
		expect(await listMcpServers()).toEqual([]);

		invoke.mockResolvedValueOnce([
			makeMcpServerSnapshot({ last_error: null, diagnostic: null, last_seen_at: null }),
		]);
		const [server] = await listMcpServers();
		expect(server.lastError).toBeNull();
		expect(server.diagnostic).toBeNull();
		expect(server.lastSeenAt).toBeNull();
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
		invoke.mockResolvedValueOnce({ added: [], removed: [], updated: [], failed: [] });
		invoke.mockResolvedValue(undefined);

		await refreshMcpServers();
		await setSkillEnabled({ name: 'example-skill', enabled: false });
		await refreshSkills();
		await openSkillsDir();
		await addMcpServer(config);
		await updateMcpServer(updateRequest);
		await removeMcpServer(nameRequest);
		await reconnectMcpServer(nameRequest);
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
			['reconnect_mcp_server', nameRequest],
			['toggle_mcp_server', { name: 'local-server', enabled: true }],
			['set_tool_enabled', enabledRequest],
		]);
		expect(invoke.mock.calls[4][1].config).toBe(config);
		expect(invoke.mock.calls[5][1]).toBe(updateRequest);
	});

	it('validates MCP refresh profile identities and keeps the Skills path inside the backend', async () => {
		const response = {
			added: ['new-server'],
			removed: [],
			updated: ['changed-server'],
			failed: ['offline-server'],
			future_field: { retained: true },
		};
		invoke.mockResolvedValueOnce(response);

		expect(await refreshMcpServers()).toEqual({
			added: ['new-server'],
			removed: [],
			updated: ['changed-server'],
			failed: ['offline-server'],
		});
		expect(invoke).toHaveBeenCalledWith('refresh_mcp_servers');
		invoke.mockResolvedValueOnce(undefined);
		expect(await openSkillsDir()).toBeUndefined();
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

	it.each([
		['missing category', { added: [], removed: [], updated: [] }],
		['invalid profile name', { added: ['invalid name'], removed: [], updated: [], failed: [] }],
		[
			'duplicate profile name',
			{ added: ['same', 'same'], removed: [], updated: [], failed: [] },
		],
		['non-string category entry', { added: [1], removed: [], updated: [], failed: [] }],
	])('rejects MCP refresh results with invalid %s data', async (_reason, result) => {
		invoke.mockResolvedValueOnce(result);

		await expect(refreshMcpServers()).rejects.toThrow('Invalid MCP refresh response');
	});

	it('allows a changed MCP profile to be both updated and failed', async () => {
		invoke.mockResolvedValueOnce({
			added: [],
			removed: [],
			updated: ['changed-server'],
			failed: ['changed-server'],
		});

		expect(await refreshMcpServers()).toEqual({
			added: [],
			removed: [],
			updated: ['changed-server'],
			failed: ['changed-server'],
		});
	});

	it('executes Skills through the typed command boundary and preserves confirmation errors', async () => {
		const request = { name: 'meeting-notes', params: { topic: 'planning' } };
		const response = {
			success: true,
			output: { summary: 'ready' },
			error: null,
		};
		invoke.mockResolvedValueOnce(response);

		expect(await executeSkill(request)).toEqual(response);
		expect(invoke).toHaveBeenCalledWith('execute_skill', request);

		const confirmationError = new Error('{"requires_confirmation":true}');
		invoke.mockReset();
		invoke.mockRejectedValueOnce(confirmationError);
		await expect(executeSkill(request)).rejects.toBe(confirmationError);
		expect(invoke).toHaveBeenCalledWith('execute_skill', request);
	});
});
