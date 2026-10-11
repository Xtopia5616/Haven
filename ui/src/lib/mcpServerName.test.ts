import { describe, expect, it } from 'vitest';
import { isValidMcpServerName } from './mcpServerName.ts';

describe('MCP server profile names', () => {
	it('accepts 1-128 ASCII letters, digits, hyphens, and underscores', () => {
		expect(isValidMcpServerName('server_1-prod')).toBe(true);
		expect(isValidMcpServerName('a'.repeat(128))).toBe(true);
	});

	it.each(['', 'a'.repeat(129), 'server.name', ' server', '服务器'])(
		'rejects invalid names: %s',
		(name) => expect(isValidMcpServerName(name)).toBe(false),
	);
});
