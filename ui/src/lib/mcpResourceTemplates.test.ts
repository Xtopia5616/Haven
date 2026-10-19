import { describe, expect, it } from 'vitest';
import { expandMcpResourceTemplate, parseMcpResourceTemplate } from './mcpResourceTemplates.ts';

describe('MCP resource URI templates', () => {
	it('extracts unique variables from RFC 6570 expressions', () => {
		expect(parseMcpResourceTemplate('mcp://demo/items/{item_id}{?limit,locale}')).toEqual({
			template: 'mcp://demo/items/{item_id}{?limit,locale}',
			variableNames: ['item_id', 'limit', 'locale'],
		});
	});

	it('expands path and query values with RFC 6570 escaping', () => {
		expect(
			expandMcpResourceTemplate('mcp://demo/items/{item_id}{?search,limit}', {
				item_id: 'a/b',
				search: 'hello world',
				limit: '10',
			}),
		).toBe('mcp://demo/items/a%2Fb?search=hello%20world&limit=10');
	});

	it('supports reserved expansion, prefixes, and empty named values', () => {
		expect(expandMcpResourceTemplate('{+path}', { path: 'a/b c' })).toBe('a/b%20c');
		expect(expandMcpResourceTemplate('{name:3}', { name: 'λambda' })).toBe('%CE%BBam');
		expect(expandMcpResourceTemplate('{;empty}', { empty: '' })).toBe(';empty');
	});

	it('rejects malformed templates and undeclared inputs', () => {
		expect(() => parseMcpResourceTemplate('mcp://demo/items/{item_id')).toThrow(
			'未配对的左花括号',
		);
		expect(() => parseMcpResourceTemplate('mcp://demo/items/{?}')).toThrow('空表达式');
		expect(() => expandMcpResourceTemplate('mcp://demo/items/{item_id}', { other: '1' })).toThrow(
			'未声明变量',
		);
	});
});
