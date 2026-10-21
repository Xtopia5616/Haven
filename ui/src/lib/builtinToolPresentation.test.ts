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
		label: name,
		desc: `${name} description`,
		risk: 'safe',
		category: overrides.category ?? 'system',
		root: name.split('.')[0],
		rootLabel: name.split('.')[0],
		rootDescription: `${name.split('.')[0]} capabilities`,
		rootIcon: 'tools',
		operation: null,
		schema: {},
		enabled: true,
		available: true,
		availabilityReason: null,
		manifest: {
			identity: {
				source: 'builtin',
				catalogGroup: overrides.category ?? 'system',
				root: overrides.root ?? name.split('.')[0],
				operation: overrides.operation ?? null,
				stableName: name,
			},
			model: { name, description: `${name} description`, inputSchema: {} },
			policy: {
				riskLevel: 'safe',
				permissionKey: name,
				confirmation: 'none',
				idempotency: 'idempotent',
				scope: 'session',
				concurrency: 'read_only',
				effect: 'read_only',
				dataSensitivity: 'none',
				networkAccess: 'none',
			},
			presentation: {
				label: name,
				renderer: 'tools',
				icon: 'tools',
				representedSource: 'builtin',
			},
			rootPresentation: {
				label: overrides.rootLabel ?? name.split('.')[0],
				description: '',
				icon: 'tools',
			},
			prompt: { whenToUse: '', whenNotToUse: '', keyOperations: [] },
			availability: {
				enabled: true,
				available: true,
				availabilityReason: null,
				requiresConnection: false,
				requiresPermission: false,
			},
		},
		...overrides,
	};
}

describe('builtin tool presentation', () => {
	it('groups builtins by the shared catalog category', () => {
		const filesRead = tool('files.read', { category: 'files' });
		const workflow = tool('agent.list', { category: 'workflow' });
		const shell = tool('shell', { category: 'shell' });
		const filesSearch = tool('files.search', { enabled: false, category: 'files' });
		const ask = tool('ask', { category: 'interaction' });
		const web = tool('web_search_api', { category: 'network' });
		const mcp = tool('mcp__calendar__list', { category: 'extensions' });

		const cards = groupBuiltinTools([filesRead, workflow, shell, filesSearch, ask, web, mcp]);

		expect(cards).toHaveLength(6);
		expect(cards[0]).toMatchObject({
			kind: 'family-group',
			name: 'interaction',
			label: '用户交互',
			roots: [{ name: 'ask', operations: [ask] }],
		});
		expect(cards[1]).toMatchObject({
			kind: 'family-group',
			name: 'workflow',
			label: '工作流',
			roots: [{ name: 'agent', operations: [workflow] }],
		});
		expect(cards[2]).toMatchObject({
			kind: 'family-group',
			name: 'files',
			label: '文件系统',
			roots: [{ name: 'files', operations: [filesRead, filesSearch] }],
		});
		expect(cards[3]).toMatchObject({
			kind: 'family-group',
			name: 'network',
			label: '网络与搜索',
			roots: [{ name: 'web_search_api', operations: [web] }],
		});
		expect(cards[4]).toMatchObject({
			kind: 'family-group',
			name: 'shell',
			label: '命令行',
			roots: [{ name: 'shell', operations: [shell] }],
		});
		expect(cards[5]).toMatchObject({
			kind: 'family-group',
			name: 'extensions',
			label: '工具与扩展',
			roots: [{ name: 'mcp__calendar__list', operations: [mcp] }],
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

	it('groups memory operations under the Haven root while retaining their operation names', () => {
		const memorySearch = tool('memory.search', {
			category: 'haven',
			root: 'haven',
			rootLabel: 'Haven',
			operation: 'memory.search',
		});
		const havenStatus = tool('haven.diagnostics.status', {
			category: 'haven',
			root: 'haven',
			rootLabel: 'Haven',
			operation: 'diagnostics.status',
		});

		const [group] = groupBuiltinTools([memorySearch, havenStatus]);

		expect(group.roots).toHaveLength(1);
		expect(group.roots[0]).toMatchObject({
			name: 'haven',
			label: 'Haven',
			operations: [havenStatus, memorySearch],
		});
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
