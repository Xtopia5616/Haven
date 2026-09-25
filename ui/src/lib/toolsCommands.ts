import { invoke } from '$lib/tauri.ts';
import type {
	McpServerSnapshot,
	SkillInfo,
	ToolListResponse,
} from './contracts/tools.ts';

/** Read builtin tool manifests using their existing Rust wire shape. */
export function getTools(): Promise<ToolListResponse> {
	return invoke('get_tools');
}

/** Read MCP catalog snapshots without interpreting open status variants. */
export function listMcpTools(): Promise<McpServerSnapshot[]> {
	return invoke('list_mcp_tools');
}

/** Read the current Skill metadata projection. */
export function listSkills(): Promise<SkillInfo[]> {
	return invoke('list_skills');
}

/** Clear local tool circuit state. */
export function resetToolCircuits(): Promise<void> {
	return invoke('reset_tool_circuits');
}
