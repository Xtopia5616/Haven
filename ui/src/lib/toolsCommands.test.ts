import { beforeEach, describe, expect, it, vi } from 'vitest';
import { getTools, listMcpTools, listSkills, resetToolCircuits } from './toolsCommands.ts';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock('$lib/tauri.ts', () => ({ invoke }));

describe('ToolsView command boundary', () => {
	beforeEach(() => {
		invoke.mockReset();
	});

	it('forwards tool manifests, empty catalog lists, and unknown MCP fields unchanged', async () => {
		const manifestResponse = {
			tools: [{ identity: { stable_name: 'files.read', extension: { source: 'future' } } }],
			extension_response: true,
		};
		const mcpResponse = [
			{
				name: 'future-server',
				status: { FutureStatus: { note: 'retained' } },
				tools: [{ name: 'future-tool', input_schema: { type: 'object', extra: true } }],
				future_field: ['retained'],
			},
		];
		const emptySkills: unknown[] = [];
		invoke
			.mockResolvedValueOnce(manifestResponse)
			.mockResolvedValueOnce(mcpResponse)
			.mockResolvedValueOnce(emptySkills);

		const manifests = await getTools();
		const servers = await listMcpTools();
		const skills = await listSkills();

		expect(invoke).toHaveBeenNthCalledWith(1, 'get_tools');
		expect(invoke).toHaveBeenNthCalledWith(2, 'list_mcp_tools');
		expect(invoke).toHaveBeenNthCalledWith(3, 'list_skills');
		expect(manifests).toBe(manifestResponse);
		expect(servers).toBe(mcpResponse);
		expect(servers[0].status).toEqual({ FutureStatus: { note: 'retained' } });
		expect(servers[0].future_field).toEqual(['retained']);
		expect(servers[0].tools[0].input_schema).toEqual({ type: 'object', extra: true });
		expect(skills).toBe(emptySkills);
	});

	it('keeps circuit reset as a void command and propagates its rejection', async () => {
		const failure = new Error('reset failed');
		invoke.mockRejectedValueOnce(failure);

		await expect(resetToolCircuits()).rejects.toBe(failure);
		expect(invoke).toHaveBeenCalledWith('reset_tool_circuits');
	});
});
