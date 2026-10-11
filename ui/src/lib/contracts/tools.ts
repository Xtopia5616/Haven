/** Tools command types; Skills/MCP response projections live in their domain contracts. */
import type {
	McpServerConfig as GeneratedMcpServerConfig,
	BuiltinToolManifestListResponse as GeneratedBuiltinToolManifestListResponse,
	SkillExecutionResponse as GeneratedSkillExecutionResponse,
	TauriCommandRequest,
} from './generatedCommands.ts';
import type { SkillInfoView } from './skills.ts';

export type BuiltinToolManifestListResponse = GeneratedBuiltinToolManifestListResponse;
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
