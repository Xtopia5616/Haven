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
import { toolRootName } from './toolManifest.ts';

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
	// The event/catalog discriminator is authoritative for new calls. The
	// payload-shape branches below remain only for legacy/resumed messages.
	if (kind === 'custom' && resultRenderer === 'files.search') return ToolFileSearchResult;
	const rootToolName = resultRenderer || toolRootName(toolName);
	if (
		kind === 'custom' &&
		isMediaOperationResult(_data) &&
		rootToolName !== 'media'
	)
		return ToolMediaResult;
	if (kind === 'custom' && rootToolName === 'agent') return ToolAgentResult;
	if (kind === 'custom' && rootToolName === 'process') return ToolProcessResult;
	if (kind === 'custom' && rootToolName === 'clipboard') return ToolClipboardResult;
	if (kind === 'custom' && rootToolName === 'input') return ToolInputResult;
	if (kind === 'custom' && rootToolName === 'window') return ToolWindowResult;
	if (kind === 'custom' && rootToolName === 'actions') return ToolActionResult;
	if (kind === 'custom' && rootToolName === 'schedule') return ToolScheduleResult;
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
	if (
		kind === 'custom' &&
		['haven_diagnostics', 'haven_config', 'haven_skills', 'haven_tools', 'haven_mcp'].includes(
			rootToolName,
		)
	)
		return ToolAdminResult;
	if (kind === 'custom' && rootToolName === 'http') return ToolHttpResult;
	if (kind === 'custom' && rootToolName === 'web_search') return ToolWebSearchResult;
	if (kind === 'custom' && rootToolName === 'memory') return ToolMemoryResult;
	if (kind === 'custom' && rootToolName === 'media') return ToolMediaResult;
	return kind ? renderers[kind as keyof typeof renderers] ?? null : null;
}

function isMediaOperationResult(value: unknown): boolean {
	if (typeof value !== 'object' || value === null || Array.isArray(value)) return false;
	const data = value as { operation?: unknown; asset_id?: unknown; media?: unknown };
	return (
		typeof data.operation === 'string' &&
		['inspect', 'describe', 'ocr', 'transcribe', 'extract', 'generate', 'record', 'play', 'speak', 'volume_get', 'volume_set', 'mute_get', 'mute_set'].includes(data.operation) &&
		(typeof data.asset_id === 'string' || (typeof data.media === 'object' && data.media !== null))
	);
}
