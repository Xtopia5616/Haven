import { describe, expect, it } from 'vitest';
import {
	TAURI_COMMAND_CONTRACTS,
	TAURI_COMMAND_NAMES,
	type CommandBoundary,
} from './commands.ts';

describe('Tauri command contract directory', () => {
	it('contains the complete unique command set', () => {
		expect(TAURI_COMMAND_NAMES).toHaveLength(71);
		expect(new Set(TAURI_COMMAND_NAMES).size).toBe(71);
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

	it('keeps memory fact and recall command contracts named', () => {
		expect(TAURI_COMMAND_CONTRACTS.recall_memory).toMatchObject({
			request: 'RecallMemoryRequest',
			response: 'MemoryRecallItem[]',
		});
		expect(TAURI_COMMAND_CONTRACTS.list_facts).toMatchObject({
			request: 'ListFactsRequest',
			response: 'Fact[]',
		});
		expect(TAURI_COMMAND_CONTRACTS.add_fact).toMatchObject({
			request: 'AddFactRequest',
			response: 'Fact',
		});
		expect(TAURI_COMMAND_CONTRACTS.delete_fact).toMatchObject({
			request: 'DeleteFactRequest',
			response: 'void',
		});
	});

	it('keeps session history and resume command contracts named', () => {
		for (const command of [
			'get_history',
			'search_history_paginated',
			'search_history',
			'search_history_filtered',
		]) {
			expect(TAURI_COMMAND_CONTRACTS[command as keyof typeof TAURI_COMMAND_CONTRACTS].response).toBe(
				'SessionHistoryRow[]',
			);
		}
		expect(TAURI_COMMAND_CONTRACTS.search_history_filtered.request).toBe('HistoryFilterRequest');
		expect(TAURI_COMMAND_CONTRACTS.get_sessions.response).toBe('SessionListResponse');
		expect(TAURI_COMMAND_CONTRACTS.get_session_for_resume).toMatchObject({
			request: 'SessionIdRequest',
			response: 'SessionResumeResponse',
		});
		expect(TAURI_COMMAND_CONTRACTS.get_last_conversation.response).toBe(
			'SessionResumeResponse | null',
		);
	});
});
