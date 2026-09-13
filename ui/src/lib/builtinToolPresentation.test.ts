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
		schema: {},
		enabled: true,
		...overrides,
	};
}

describe('builtin tool presentation', () => {
	it('groups operation views by root while preserving standalone builtins', () => {
		const filesRead = tool('files.read');
		const shell = tool('shell');
		const filesSearch = tool('files.search', { enabled: false });

		const cards = groupBuiltinTools([filesRead, shell, filesSearch]);

		expect(cards).toHaveLength(2);
		expect(cards[0]).toMatchObject({
			kind: 'operation-group',
			name: 'files',
			label: '文件与搜索',
			operations: [filesRead, filesSearch],
		});
		expect(cards[1]).toEqual({ kind: 'single', tool: shell });
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
