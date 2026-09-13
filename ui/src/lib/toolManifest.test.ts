import { afterEach, describe, expect, it } from 'vitest';
import ToolAdminResult from './ToolAdminResult.svelte';
import ToolFileSearchResult from './ToolFileSearchResult.svelte';
import { parseToolResult } from './toolResultParsing.ts';
import { getToolResultRenderer } from './toolResultRenderers.ts';
import {
	getToolManifest,
	setToolManifests,
	toolRendererName,
	toolRootName,
} from './toolManifest.ts';

function manifest(stableName: string, root: string, renderer: string) {
	return {
		manifest: {
			identity: { stable_name: stableName, root },
			presentation: { label: stableName, renderer, icon: 'tools' },
		},
	};
}

afterEach(() => {
	setToolManifests([]);
});

describe('tool manifest snapshots', () => {
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

	it('uses the manifest renderer for haven operations while keeping root haven', () => {
		setToolManifests([manifest('haven.diagnostics.status', 'haven', 'settings')]);
		const data = { status: 'ok', version: '0.1.0' };

		expect(toolRootName('haven.diagnostics.status')).toBe('haven');
		expect(parseToolResult('haven.diagnostics.status', JSON.stringify(data))).toMatchObject({
			kind: 'custom',
		});
		expect(getToolResultRenderer('custom', 'haven.diagnostics.status', data)).toBe(
			ToolAdminResult,
		);
	});
});
