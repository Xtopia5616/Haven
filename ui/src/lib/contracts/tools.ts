/** Tools command types; Skills/MCP response projections live in their domain contracts. */
import type {
	McpServerConfig as GeneratedMcpServerConfig,
	SkillExecutionResponse as GeneratedSkillExecutionResponse,
	TauriCommandRequest,
} from './generatedCommands.ts';
import { isRecord } from './objectGuards.ts';
import type { SkillInfoView } from './skills.ts';

/** The envelope is validated here; each dynamic manifest is parsed by toolManifest.ts. */
export interface BuiltinToolManifestListView {
	tools: unknown[];
	[key: string]: unknown;
}

export function parseBuiltinToolManifestList(value: unknown): BuiltinToolManifestListView {
	if (!isRecord(value) || !Array.isArray(value.tools)) {
		throw new Error('Invalid builtin tool manifest list response');
	}
	return { ...value, tools: value.tools };
}

export type SkillInfo = SkillInfoView;
export type SkillExecutionResponse = GeneratedSkillExecutionResponse;
export type McpServerConfig = GeneratedMcpServerConfig;
export type McpServerConfigInput = TauriCommandRequest<'add_mcp_server'>['config'];
export type ExecuteSkillRequest = TauriCommandRequest<'execute_skill'>;

/** Flat command arguments are aliases of the generated Rust handler shapes. */
export type ReconnectMcpServerRequest = TauriCommandRequest<'reconnect_mcp_server'>;
export type RemoveMcpServerRequest = TauriCommandRequest<'remove_mcp_server'>;
export type SetSkillEnabledRequest = TauriCommandRequest<'set_skill_enabled'>;
export type SetToolEnabledRequest = TauriCommandRequest<'set_tool_enabled'>;
export type ToggleMcpServerRequest = TauriCommandRequest<'toggle_mcp_server'>;
export type UpdateMcpServerRequest = TauriCommandRequest<'update_mcp_server'>;
