import { MCP_CLIENT_STATUS_UNIT_VALUES, type McpClientStatus } from './generatedCommands.ts';
import { isRecord } from './objectGuards.ts';
import { isOneOf, isString } from './valueGuards.ts';

/** Validate the generated Rust MCP status union at every renderer boundary. */
export function isMcpClientStatus(value: unknown): value is McpClientStatus {
	if (isOneOf(value, MCP_CLIENT_STATUS_UNIT_VALUES)) return true;
	if (!isRecord(value) || Object.keys(value).length !== 1 || !isRecord(value.Offline))
		return false;
	return Object.keys(value.Offline).length === 1 && isString(value.Offline.error);
}
