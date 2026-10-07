import ToolAgentResult from './ToolAgentResult.svelte';
import ToolFileResult from './ToolFileResult.svelte';
import ToolFileSearchResult from './ToolFileSearchResult.svelte';
import ToolJsonResult from './ToolJsonResult.svelte';
import ToolNotifyResult from './ToolNotifyResult.svelte';
import ToolShellResult from './ToolShellResult.svelte';
import ToolProcessResult from './ToolProcessResult.svelte';
import ToolRunsResult from './ToolRunsResult.svelte';
import ToolClipboardResult from './ToolClipboardResult.svelte';
import ToolHttpResult from './ToolHttpResult.svelte';
import ToolInputResult from './ToolInputResult.svelte';
import ToolAdminResult from './ToolAdminResult.svelte';
import ToolMemoryResult from './ToolMemoryResult.svelte';
import ToolMediaResult from './ToolMediaResult.svelte';
import ToolScheduleResult from './ToolScheduleResult.svelte';
import ToolSystemResult from './ToolSystemResult.svelte';
import ToolWebSearchResult from './ToolWebSearchResult.svelte';
import ToolWindowResult from './ToolWindowResult.svelte';
import { toolRendererName } from './toolManifest.ts';

const adminRendererNames: ReadonlySet<string> = new Set([
	'haven_diagnostics',
	'haven_config',
	'haven_skills',
	'haven_tools',
	'haven_mcp',
]);

const renderers = {
	shell: ToolShellResult,
	notify: ToolNotifyResult,
	generic: ToolJsonResult,
	raw: ToolJsonResult,
} as const;

/**
 * Resolve the body component for a normalized tool-result kind. This registry
 * keeps data-specific result rendering out of the shared card shell while
 * allowing each custom tool to retain its own result shape.
 */

export function getToolResultRenderer(
	kind: string | null | undefined,
	toolName = '',
	_data: unknown = null,
	resultRenderer: string | null = null,
) {
	// The event/catalog discriminator selects the result family. Payload fields
	// only split current families that deliberately share a renderer.
	const rendererName = resultRenderer || toolRendererName(toolName);
	const selectedRenderer = rendererName || '';
	if (kind === 'custom' && selectedRenderer === 'files.search') return ToolFileSearchResult;
	if (kind === 'custom' && selectedRenderer === 'agent') return ToolAgentResult;
	if (kind === 'custom' && selectedRenderer === 'process') return ToolProcessResult;
	if (kind === 'custom' && selectedRenderer === 'clipboard') return ToolClipboardResult;
	if (kind === 'custom' && selectedRenderer === 'input') return ToolInputResult;
	if (kind === 'custom' && selectedRenderer === 'window') return ToolWindowResult;
	if (kind === 'custom' && selectedRenderer === 'tool_runs') return ToolRunsResult;
	if (kind === 'custom' && selectedRenderer === 'schedule') return ToolScheduleResult;
	if (
		kind === 'custom' &&
		selectedRenderer === 'files' &&
		typeof _data === 'object' &&
		_data !== null &&
		'media' in _data
	)
		return ToolMediaResult;
	if (
		kind === 'custom' &&
		selectedRenderer === 'files' &&
		typeof _data === 'object' &&
		_data !== null &&
		!('results' in _data)
	)
		return ToolFileResult;
	if (
		kind === 'custom' &&
		selectedRenderer === 'files' &&
		typeof _data === 'object' &&
		_data !== null &&
		'results' in _data &&
		Array.isArray(_data.results)
	)
		return ToolFileSearchResult;
	if (kind === 'custom' && selectedRenderer === 'system') {
		const scope =
			typeof _data === 'object' && _data !== null && 'scope' in _data
				? (_data as { scope?: unknown }).scope
				: null;
		if (scope === 'process') return ToolProcessResult;
		if (scope === 'window') return ToolWindowResult;
		if (scope === 'clipboard') return ToolClipboardResult;
		if (scope === 'input') return ToolInputResult;
		return ToolSystemResult;
	}
	if (kind === 'custom' && adminRendererNames.has(selectedRenderer)) return ToolAdminResult;
	if (kind === 'custom' && selectedRenderer === 'http') return ToolHttpResult;
	if (kind === 'custom' && selectedRenderer === 'web_search') return ToolWebSearchResult;
	if (kind === 'custom' && selectedRenderer === 'memory') return ToolMemoryResult;
	if (kind === 'custom' && selectedRenderer === 'media') return ToolMediaResult;
	if (kind === 'custom') return ToolJsonResult;
	return kind ? (renderers[kind as keyof typeof renderers] ?? null) : null;
}
