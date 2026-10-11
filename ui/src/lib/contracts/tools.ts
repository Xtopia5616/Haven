/**
 * ToolsView command types derived from generated Rust wire DTOs.
 * MCP wire validation and renderer projections live in contracts/mcp.ts.
 */
import type {
	McpServerConfig as GeneratedMcpServerConfig,
	SkillInfo as GeneratedSkillInfo,
	SkillExecutionResponse as GeneratedSkillExecutionResponse,
	BuiltinToolManifestListResponse as GeneratedBuiltinToolManifestListResponse,
	TauriCommandRequest,
} from './generatedCommands.ts';

export type BuiltinToolManifestListResponse = GeneratedBuiltinToolManifestListResponse;
export type SkillInfo = GeneratedSkillInfo;
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
