import { describe, expect, it } from 'vitest';
import {
	parseMcpPrompt,
	parseMcpPromptList,
	parseMcpResourceList,
	parseMcpResourceRead,
} from './mcpFeatures.ts';

describe('MCP resource and prompt response contracts', () => {
	it('accepts bounded resource metadata and content with nullable cache hints', () => {
		expect(
			parseMcpResourceList({
				resources: [{ uri: 'haven://docs/intro', name: 'intro' }],
				ttlMs: null,
				cacheScope: 'private',
			}),
		).toMatchObject({ resources: [{ uri: 'haven://docs/intro' }], cacheScope: 'private' });
		expect(
			parseMcpResourceRead({
				contents: [{ uri: 'haven://docs/intro', text: 'hello' }],
				ttlMs: 1000,
				cacheScope: 'public',
			}),
		).toMatchObject({ contents: [{ text: 'hello' }], ttlMs: 1000 });
	});

	it('accepts prompt descriptions serialized as null and preserves typed message content', () => {
		expect(
			parseMcpPrompt({
				description: null,
				messages: [{ role: 'user', content: { type: 'text', text: 'Review this' } }],
				ttlMs: null,
				cacheScope: 'private',
			}),
		).toMatchObject({
			description: null,
			messages: [{ role: 'user', content: { type: 'text', text: 'Review this' } }],
		});
	});

	it('rejects malformed or duplicate server supplied identities', () => {
		expect(() => parseMcpResourceList({ resources: [], ttlMs: null, cacheScope: 'shared' })).toThrow(
			'Invalid MCP resource list',
		);
		expect(
			() =>
				parseMcpPromptList({
					prompts: [
						{ name: 'same', arguments: [] },
						{ name: 'same', arguments: [] },
					],
					ttlMs: null,
					cacheScope: 'private',
				}),
		).toThrow('Invalid MCP prompt list');
		expect(
			() =>
				parseMcpPrompt({
					messages: [{ role: 'user', content: { type: 'text', text: 'Review this' } }],
					ttlMs: null,
					cacheScope: 'private',
				}),
		).toThrow('Invalid MCP prompt response');
	});

	it('rejects oversized or structurally invalid content', () => {
		expect(
			() =>
				parseMcpResourceRead({
					contents: [{ uri: 'haven://doc', text: 'text', blob: 'YmluYXJ5' }],
					ttlMs: null,
					cacheScope: 'private',
				}),
		).toThrow('Invalid MCP resource response');
		expect(
			() =>
				parseMcpPrompt({
					description: null,
					messages: [{ role: 'system', content: { type: 'text', text: 'hidden role' } }],
					ttlMs: null,
					cacheScope: 'private',
				}),
		).toThrow('Invalid MCP prompt response');
	});

	it('caps serialized list and resource response bytes at the renderer boundary', () => {
		const largeDescription = 'd'.repeat(15_000);
		const resources = Array.from({ length: 300 }, (_, index) => ({
			uri: `mcp://demo/${index}`,
			name: `resource-${index}`,
			description: largeDescription,
		}));
		expect(() => parseMcpResourceList({ resources, ttlMs: null, cacheScope: 'private' })).toThrow(
			'Invalid MCP resource list',
		);

		const prompts = Array.from({ length: 300 }, (_, index) => ({
			name: `prompt-${index}`,
			description: largeDescription,
			arguments: [],
		}));
		expect(() => parseMcpPromptList({ prompts, ttlMs: null, cacheScope: 'private' })).toThrow(
			'Invalid MCP prompt list',
		);

		const contents = Array.from({ length: 5 }, (_, index) => ({
			uri: `mcp://demo/${index}`,
			text: 'x'.repeat(900_000),
		}));
		expect(() => parseMcpResourceRead({ contents, ttlMs: null, cacheScope: 'private' })).toThrow(
			'Invalid MCP resource response',
		);
	});
});
