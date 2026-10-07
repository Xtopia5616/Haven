/**
 * ToolsView command response contracts derived from generated Rust wire DTOs.
 * Tool manifest projections and runtime MCP status validation remain at the renderer boundary.
 */
import type {
	McpClientStatus as GeneratedMcpClientStatus,
	McpRefreshResult as GeneratedMcpRefreshResult,
	McpServerConfig as GeneratedMcpServerConfig,
	McpServerSnapshot as GeneratedMcpServerSnapshot,
	McpToolInfo as GeneratedMcpToolInfo,
	SkillInfo as GeneratedSkillInfo,
	BuiltinToolManifestListResponse as GeneratedBuiltinToolManifestListResponse,
	TauriCommandRequest,
} from './generatedCommands.ts';

export type BuiltinToolManifestListResponse = GeneratedBuiltinToolManifestListResponse;
export type SkillInfo = GeneratedSkillInfo;
export type McpClientStatus = GeneratedMcpClientStatus;
export type McpToolInfo = GeneratedMcpToolInfo;
export type McpServerSnapshot = GeneratedMcpServerSnapshot;
export type McpServerConfig = GeneratedMcpServerConfig;
export type McpServerConfigInput = TauriCommandRequest<'add_mcp_server'>['config'];
export type McpRefreshResult = GeneratedMcpRefreshResult;

/** Flat command arguments are aliases of the generated Rust handler shapes. */
export type McpNameRequest = TauriCommandRequest<'reconnect_mcp'>;
export type SetEnabledRequest = TauriCommandRequest<'set_tool_enabled'>;
export type ToggleMcpServerRequest = TauriCommandRequest<'toggle_mcp_server'>;
export type UpdateMcpServerRequest = TauriCommandRequest<'update_mcp_server'>;
