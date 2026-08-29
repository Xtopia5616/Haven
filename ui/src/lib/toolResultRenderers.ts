import ToolFileResult from './ToolFileResult.svelte';
import ToolFileSearchResult from './ToolFileSearchResult.svelte';
import ToolJsonResult from './ToolJsonResult.svelte';
import ToolNotifyResult from './ToolNotifyResult.svelte';
import ToolShellResult from './ToolShellResult.svelte';
import ToolProcessResult from './ToolProcessResult.svelte';
import ToolActionResult from './ToolActionResult.svelte';
import ToolClipboardResult from './ToolClipboardResult.svelte';
import ToolHttpResult from './ToolHttpResult.svelte';
import ToolScheduleResult from './ToolScheduleResult.svelte';
import ToolSystemResult from './ToolSystemResult.svelte';
import ToolWebSearchResult from './ToolWebSearchResult.svelte';
import ToolWindowResult from './ToolWindowResult.svelte';

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
) {
	if (kind === 'custom' && toolName === 'file') return ToolFileResult;
	if (kind === 'custom' && toolName === 'file_search') return ToolFileSearchResult;
	if (
		kind === 'custom' &&
		toolName === 'files' &&
		typeof _data === 'object' &&
		_data !== null &&
		'results' in _data &&
		Array.isArray(_data.results)
	)
		return ToolFileSearchResult;
	if (kind === 'custom' && toolName === 'system') return ToolSystemResult;
	if (kind === 'custom' && toolName === 'process') return ToolProcessResult;
	if (kind === 'custom' && toolName === 'window') return ToolWindowResult;
	if (kind === 'custom' && toolName === 'actions') return ToolActionResult;
	if (kind === 'custom' && toolName === 'schedule') return ToolScheduleResult;
	if (kind === 'custom' && toolName === 'http') return ToolHttpResult;
	if (kind === 'custom' && toolName === 'clipboard') return ToolClipboardResult;
	if (kind === 'custom' && toolName === 'web_search') return ToolWebSearchResult;
	return kind ? renderers[kind as keyof typeof renderers] ?? null : null;
}
