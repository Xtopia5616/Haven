import { describe, expect, it } from 'vitest';
import { TAURI_COMMAND_CONTRACTS, TAURI_COMMAND_NAMES, type CommandBoundary } from './commands.ts';

describe('Tauri command boundary directory', () => {
	it('matches the reviewed command count without duplicating generated shapes', () => {
		expect(TAURI_COMMAND_NAMES).toHaveLength(86);
		expect(new Set(TAURI_COMMAND_NAMES).size).toBe(86);
		expect(TAURI_COMMAND_NAMES).toEqual(Object.keys(TAURI_COMMAND_CONTRACTS));
		expect(TAURI_COMMAND_CONTRACTS.pick_execution_workspace_directory.boundary).toBe('read');
		expect(TAURI_COMMAND_CONTRACTS.pick_execution_workspace_directory.security).toContain(
			'grants no file access',
		);
		for (const contract of Object.values(TAURI_COMMAND_CONTRACTS)) {
			expect(Object.keys(contract).sort()).toEqual(['boundary', 'security']);
			expect(contract.security.trim().length).toBeGreaterThan(0);
		}
	});

	it('keeps privileged commands explicitly classified and reviewed', () => {
		const executeCommands = Object.entries(TAURI_COMMAND_CONTRACTS)
			.filter(([, contract]) => contract.boundary === ('execute' satisfies CommandBoundary))
			.map(([name]) => name);
		expect(executeCommands).toEqual(
			expect.arrayContaining([
				'open_external',
				'execute_skill',
				'process_transcript',
				'open_skills_dir',
				'list_mcp_resources',
				'read_mcp_resource',
				'list_mcp_prompts',
				'get_mcp_prompt',
			]),
		);
		expect(TAURI_COMMAND_CONTRACTS.execute_skill.security).toContain('AuthorizationEngine');
	});

	it('keeps MCP administration tied to the native authorization boundary', () => {
		for (const command of [
			'reconnect_mcp_server',
			'refresh_mcp_servers',
			'add_mcp_server',
			'update_mcp_server',
			'remove_mcp_server',
			'toggle_mcp_server',
		] as const) {
			const contract = TAURI_COMMAND_CONTRACTS[command];
			expect(contract.boundary).toBe('execute');
			expect(contract.security).toContain('AuthorizationEngine');
		}
	});
});
