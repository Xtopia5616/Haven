import { describe, expect, it } from 'vitest';
import {
	TAURI_COMMAND_CONTRACTS,
	TAURI_COMMAND_NAMES,
	type CommandBoundary,
} from './commands.ts';

describe('Tauri command contract directory', () => {
	it('contains the complete unique command set', () => {
		expect(TAURI_COMMAND_NAMES).toHaveLength(67);
		expect(new Set(TAURI_COMMAND_NAMES).size).toBe(67);
		expect(TAURI_COMMAND_NAMES).toEqual(Object.keys(TAURI_COMMAND_CONTRACTS));
	});

	it('keeps privileged commands explicitly classified', () => {
		const executeCommands = Object.entries(TAURI_COMMAND_CONTRACTS)
			.filter(([, contract]) => contract.boundary === ('execute' satisfies CommandBoundary))
			.map(([name]) => name);
		expect(executeCommands).toEqual(expect.arrayContaining([
			'open_external',
			'mcp_tool_call',
			'execute_skill',
			'process_transcript',
			'open_skills_dir',
		]));
		expect(TAURI_COMMAND_CONTRACTS.mcp_tool_call.response).toBe('McpToolCallResponse');
		expect(TAURI_COMMAND_CONTRACTS.execute_skill.response).toBe('SkillExecutionResponse');
	});
});

