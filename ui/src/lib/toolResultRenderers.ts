import ToolAgentResult from './ToolAgentResult.svelte';
import ToolFileResult from './ToolFileResult.svelte';
import ToolFileSearchResult from './ToolFileSearchResult.svelte';
import ToolJsonResult from './ToolJsonResult.svelte';
import ToolNotifyResult from './ToolNotifyResult.svelte';
import ToolShellResult from './ToolShellResult.svelte';
import ToolProcessResult from './ToolProcessResult.svelte';
import ToolActionResult from './ToolActionResult.svelte';
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
import { toolRootName } from './operationViewContract.ts';

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
	const rootToolName = toolRootName(toolName);
	if (kind === 'custom' && rootToolName === 'agent') return ToolAgentResult;
	if (
		kind === 'custom' &&
		rootToolName === 'files' &&
		typeof _data === 'object' &&
		_data !== null &&
		'media' in _data
	)
		return ToolMediaResult;
	if (
		kind === 'custom' &&
		rootToolName === 'files' &&
		typeof _data === 'object' &&
		_data !== null &&
		!('results' in _data)
	)
		return ToolFileResult;
	if (
		kind === 'custom' &&
		rootToolName === 'files' &&
		typeof _data === 'object' &&
		_data !== null &&
		'results' in _data &&
		Array.isArray(_data.results)
	)
		return ToolFileSearchResult;
	if (kind === 'custom' && rootToolName === 'system') {
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
	if (kind === 'custom' && rootToolName === 'haven') {
		const operation =
			typeof _data === 'object' && _data !== null && 'operation' in _data
				? (_data as { operation?: unknown }).operation
				: null;
		if (typeof operation === 'string' && operation.startsWith('actions_')) {
			return ToolActionResult;
		}
		if (typeof operation === 'string' && operation.startsWith('schedule_')) {
			return ToolScheduleResult;
		}
		return ToolAdminResult;
	}
	if (kind === 'custom' && rootToolName === 'http') return ToolHttpResult;
	if (kind === 'custom' && rootToolName === 'web_search') return ToolWebSearchResult;
	if (kind === 'custom' && rootToolName === 'memory') return ToolMemoryResult;
	if (kind === 'custom' && rootToolName === 'media') return ToolMediaResult;
	return kind ? renderers[kind as keyof typeof renderers] ?? null : null;
}
