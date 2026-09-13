import { describe, expect, it } from 'vitest';
import {
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
			kind: 'category-group',
			name: 'haven',
			label: 'Haven',
			operations: [ask],
		});
		expect(cards[1]).toMatchObject({
			kind: 'category-group',
			name: 'system',
			label: '系统',
			operations: [filesRead, shell, filesSearch],
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
});
