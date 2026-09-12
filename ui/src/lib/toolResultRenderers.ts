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
import ToolAudioResult from './ToolAudioResult.svelte';
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
	if (kind === 'custom' && rootToolName === 'system') return ToolSystemResult;
	if (kind === 'custom' && rootToolName === 'process') return ToolProcessResult;
	if (kind === 'custom' && rootToolName === 'window') return ToolWindowResult;
	if (kind === 'custom' && rootToolName === 'actions') return ToolActionResult;
	if (kind === 'custom' && rootToolName === 'schedule') return ToolScheduleResult;
	if (kind === 'custom' && rootToolName === 'http') return ToolHttpResult;
	if (kind === 'custom' && rootToolName === 'clipboard') return ToolClipboardResult;
	if (kind === 'custom' && rootToolName === 'web_search') return ToolWebSearchResult;
	if (kind === 'custom' && rootToolName === 'memory') return ToolMemoryResult;
	if (kind === 'custom' && rootToolName === 'input') return ToolInputResult;
	if (kind === 'custom' && rootToolName === 'audio') return ToolAudioResult;
	if (kind === 'custom' && rootToolName === 'media') return ToolMediaResult;
	if (
		kind === 'custom' &&
		['haven_config', 'haven_diagnostics', 'haven_mcp', 'haven_skills', 'haven_tools'].includes(rootToolName)
	)
		return ToolAdminResult;
	return kind ? renderers[kind as keyof typeof renderers] ?? null : null;
}
