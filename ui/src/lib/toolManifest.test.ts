import { afterEach, describe, expect, it } from 'vitest';
import ToolAdminResult from './ToolAdminResult.svelte';
import ToolFileSearchResult from './ToolFileSearchResult.svelte';
import ToolJsonResult from './ToolJsonResult.svelte';
import { parseToolResult } from './toolResultParsing.ts';
import { getToolResultRenderer } from './toolResultRenderers.ts';
import {
	getToolManifest,
	parseToolManifest,
	setToolManifests,
	toolRendererName,
	toolRootName,
} from './toolManifest.ts';

function manifest(stableName: string, root: string, renderer: string) {
	return {
		identity: {
			source: 'builtin',
			catalog_group: 'system',
			root,
			operation: stableName.includes('.') ? stableName.split('.').at(-1) : null,
			stable_name: stableName,
		},
		model: {
			name: stableName,
			description: `${stableName} description`,
			input_schema: { type: 'object' },
		},
		policy: {
			risk_level: 'safe',
			permission_key: stableName,
			confirmation: 'none',
			idempotency: 'idempotent',
			scope: 'session',
			concurrency: 'read_only',
			effect: 'read_only',
			data_sensitivity: 'none',
			network_access: 'none',
		},
		presentation: {
			label: stableName,
			renderer,
			icon: 'tools',
			represented_source: 'builtin',
		},
		root_presentation: {
			label: root,
			description: `${root} capabilities`,
			icon: 'tools',
		},
		prompt: {
			when_to_use: `Use ${stableName}`,
			when_not_to_use: 'Never',
			key_operations: [stableName],
		},
		availability: {
			enabled: true,
			available: true,
			availability_reason: null,
			requires_connection: false,
			requires_permission: false,
		},
	};
}

afterEach(() => {
	setToolManifests([]);
});

describe('tool manifest snapshots', () => {
	it('converts the IPC snake_case payload at the boundary', () => {
		const parsed = parseToolManifest({
			identity: {
				source: 'builtin',
				catalog_group: 'system',
				root: 'files',
				operation: 'read',
				stable_name: 'files.read',
			},
			model: { name: 'files.read', description: 'Read', input_schema: { type: 'object' } },
			policy: {
				risk_level: 'low',
				permission_key: 'files.read',
				confirmation: 'none',
				idempotency: 'idempotent',
				scope: 'session',
				concurrency: 'read_only',
				effect: 'read_only',
				data_sensitivity: 'none',
				network_access: 'none',
			},
			presentation: {
				label: '读取文件',
				renderer: 'files',
				icon: 'file',
				represented_source: 'builtin',
			},
			root_presentation: { label: '文件', description: '文件操作', icon: 'file' },
			prompt: {
				when_to_use: 'read',
				when_not_to_use: 'write',
				key_operations: ['files.read'],
			},
			availability: {
				enabled: true,
				available: true,
				requires_connection: false,
				requires_permission: false,
			},
		});
		expect(parsed?.identity.catalogGroup).toBe('system');
		expect(parsed?.identity.stableName).toBe('files.read');
		expect(parsed?.model.inputSchema).toEqual({ type: 'object' });
	});

	it('replaces the snapshot instead of retaining removed manifests', () => {
		setToolManifests([
			manifest('files.read', 'files', 'files'),
			manifest('files.search', 'files', 'files.search'),
		]);
		expect(getToolManifest('files.read')).not.toBeNull();

		setToolManifests([manifest('files.search', 'files', 'files.search')]);

		expect(getToolManifest('files.read')).toBeNull();
		expect(getToolManifest('files.search')).not.toBeNull();
	});

	it('keeps the grouping root independent from the result renderer', () => {
		setToolManifests([manifest('files.search', 'files', 'files.search')]);

		expect(toolRootName('files.search')).toBe('files');
		expect(toolRendererName('files.search')).toBe('files.search');
	});

	it('clears the snapshot when the backend returns an invalid catalog value', () => {
		setToolManifests([manifest('files.read', 'files', 'files')]);
		setToolManifests(null);

		expect(getToolManifest('files.read')).toBeNull();
	});

	it('rejects wrapper and partial manifest payloads', () => {
		setToolManifests([
			{
				name: 'files.read',
				catalog_group: 'system',
				manifest: { presentation: { label: '读取文件' } },
			},
			{
				name: 'shell',
				catalog_group: 'system',
				risk_level: 'high',
			},
		]);
		expect(getToolManifest('files.read')).toBeNull();
		expect(getToolManifest('shell')).toBeNull();
	});
});

describe('manifest-driven result renderers', () => {
	it('uses the files.search renderer without a static operation mapping', () => {
		setToolManifests([manifest('files.search', 'files', 'files.search')]);
		const data = { results: [{ path: 'src/main.rs', line: 4 }] };

		expect(parseToolResult('files.search', JSON.stringify(data))).toMatchObject({
			kind: 'custom',
		});
		expect(getToolResultRenderer('custom', 'files.search', data)).toBe(ToolFileSearchResult);
	});

	it('uses the canonical admin renderer while keeping the manifest root', () => {
		setToolManifests([manifest('haven.diagnostics.status', 'haven', 'haven_diagnostics')]);
		const data = { status: 'ok', version: '0.1.0' };

		expect(toolRootName('haven.diagnostics.status')).toBe('haven');
		expect(parseToolResult('haven.diagnostics.status', JSON.stringify(data))).toMatchObject({
			kind: 'custom',
		});
		expect(getToolResultRenderer('custom', 'haven.diagnostics.status', data)).toBe(
			ToolAdminResult,
		);
	});

	it('renders an unknown extension renderer as generic JSON', () => {
		setToolManifests([manifest('custom.operation', 'custom', 'custom_extension')]);
		expect(parseToolResult('custom.operation', JSON.stringify({ value: 1 }))).toMatchObject({
			kind: 'custom',
		});
		expect(getToolResultRenderer('custom', 'custom.operation', { value: 1 })).toBe(
			ToolJsonResult,
		);
	});

	it('keeps MCP results generic when a server name collides with a built-in renderer', () => {
		expect(
			getToolResultRenderer('custom', 'mcp__agent__list', { agents: [] }, 'mcp'),
		).toBe(ToolJsonResult);
	});
});
