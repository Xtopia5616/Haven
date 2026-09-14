import { describe, expect, it } from 'vitest';
import {
	filterBuiltinToolCard,
	groupBuiltinTools,
	matchesBuiltinToolCard,
	type BuiltinToolEntry,
} from './builtinToolPresentation.ts';

function tool(name: string, overrides: Partial<BuiltinToolEntry> = {}): BuiltinToolEntry {
	return {
		name,
		desc: `${name} description`,
		risk: 'safe',
		category: 'system',
		schema: {},
		enabled: true,
		...overrides,
	};
}

describe('builtin tool presentation', () => {
	it('groups builtins by the shared catalog category', () => {
		const filesRead = tool('files.read');
		const shell = tool('shell');
		const filesSearch = tool('files.search', { enabled: false });
		const ask = tool('ask', { category: 'haven' });

		const cards = groupBuiltinTools([filesRead, shell, filesSearch, ask]);

		expect(cards).toHaveLength(2);
		expect(cards[0]).toMatchObject({
			kind: 'family-group',
			name: 'haven',
			label: 'Haven',
			roots: [{ name: 'ask', operations: [ask] }],
		});
		expect(cards[1]).toMatchObject({
			kind: 'family-group',
			name: 'system',
			label: 'System',
			roots: [
				{ name: 'files', operations: [filesRead, filesSearch] },
				{ name: 'shell', operations: [shell] },
			],
		});
	});

	it('keeps a mixed operation group visible for either status filter', () => {
		const [group] = groupBuiltinTools([
			tool('files.read'),
			tool('files.delete', { enabled: false }),
		]);

		expect(matchesBuiltinToolCard(group, '', 'all')).toBe(true);
		expect(matchesBuiltinToolCard(group, '', 'enabled')).toBe(true);
		expect(matchesBuiltinToolCard(group, '', 'disabled')).toBe(true);
	});

	it('searches operation names and descriptions without changing the card shape', () => {
		const [group] = groupBuiltinTools([
			tool('files.read'),
			tool('files.search', { desc: 'Search source content' }),
		]);

		expect(matchesBuiltinToolCard(group, 'files.search', 'all')).toBe(true);
		expect(matchesBuiltinToolCard(group, 'source content', 'all')).toBe(true);
		expect(matchesBuiltinToolCard(group, 'missing', 'all')).toBe(false);
	});

	it('uses manifest roots to preserve the backend capability hierarchy', () => {
		const [group] = groupBuiltinTools([
			tool('system.env.get', { root: 'system', operation: 'env.get' }),
			tool('system.registry.get', { root: 'system', operation: 'registry.get' }),
			tool('files.read', { root: 'files', operation: 'read' }),
		]);

		expect(group.roots.map((root) => root.name)).toEqual(['files', 'system']);
		expect(group.roots.find((root) => root.name === 'system')?.operations).toHaveLength(2);
	});

	it('filters each level of the tree while retaining matching descendants', () => {
		const [group] = groupBuiltinTools([
			tool('files.read'),
			tool('files.search', { desc: 'Search source content' }),
			tool('shell'),
		]);

		const filtered = filterBuiltinToolCard(group, 'source content', 'all');
		expect(filtered?.roots).toHaveLength(1);
		expect(filtered?.roots[0]).toMatchObject({
			name: 'files',
			operations: [expect.objectContaining({ name: 'files.search' })],
		});
	});
});
