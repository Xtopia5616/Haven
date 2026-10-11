/** Shared UI boundary rule for exact MCP server profile names. */
export function isValidMcpServerName(name: string): boolean {
	return /^[A-Za-z0-9_-]{1,128}$/.test(name);
}
