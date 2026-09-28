import { describe, expect, it } from 'vitest';
import { TAURI_COMMAND_CONTRACTS, TAURI_COMMAND_NAMES, type CommandBoundary } from './commands.ts';

describe('Tauri command boundary directory', () => {
	it('contains the complete unique command set without duplicating generated shapes', () => {
		expect(TAURI_COMMAND_NAMES).toHaveLength(71);
		expect(new Set(TAURI_COMMAND_NAMES).size).toBe(71);
		expect(TAURI_COMMAND_NAMES).toEqual(Object.keys(TAURI_COMMAND_CONTRACTS));
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
				'mcp_tool_call',
				'execute_skill',
				'process_transcript',
				'open_skills_dir',
			]),
		);
		expect(TAURI_COMMAND_CONTRACTS.mcp_tool_call.security).toContain('AuthorizationEngine');
		expect(TAURI_COMMAND_CONTRACTS.execute_skill.security).toContain('AuthorizationEngine');
	});

	it('keeps MCP administration tied to the native authorization boundary', () => {
		for (const command of [
			'reconnect_mcp',
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
