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
import { isRecord } from './contracts/objectGuards.ts';
import { isValidBuiltinToolResultData } from './toolResultValidation.ts';

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

type BuiltinResultRenderer =
	| typeof ToolAgentResult
	| typeof ToolFileResult
	| typeof ToolFileSearchResult
	| typeof ToolProcessResult
	| typeof ToolClipboardResult
	| typeof ToolInputResult
	| typeof ToolWindowResult
	| typeof ToolRunsResult
	| typeof ToolScheduleResult
	| typeof ToolSystemResult
	| typeof ToolAdminResult
	| typeof ToolHttpResult
	| typeof ToolWebSearchResult
	| typeof ToolMemoryResult
	| typeof ToolMediaResult;

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
	const data = isRecord(_data) ? _data : null;
	if (kind === 'custom') {
		let renderer: BuiltinResultRenderer | null = null;
		let contractName = selectedRenderer;
		if (selectedRenderer === 'files.search') {
			renderer = ToolFileSearchResult;
			contractName = 'files.search';
		} else if (selectedRenderer === 'agent') renderer = ToolAgentResult;
		else if (selectedRenderer === 'process') renderer = ToolProcessResult;
		else if (selectedRenderer === 'clipboard') renderer = ToolClipboardResult;
		else if (selectedRenderer === 'input') renderer = ToolInputResult;
		else if (selectedRenderer === 'window') renderer = ToolWindowResult;
		else if (selectedRenderer === 'tool_runs') renderer = ToolRunsResult;
		else if (selectedRenderer === 'schedule') renderer = ToolScheduleResult;
		else if (selectedRenderer === 'files') {
			if (data && 'media' in data) {
				renderer = ToolMediaResult;
				contractName = 'media';
			} else if (data && 'results' in data) {
				renderer = ToolFileSearchResult;
				contractName = 'files.search';
			} else renderer = ToolFileResult;
		} else if (selectedRenderer === 'system') {
			const scope = data?.scope;
			if (scope === 'process') renderer = ToolProcessResult;
			else if (scope === 'window') renderer = ToolWindowResult;
			else if (scope === 'clipboard') renderer = ToolClipboardResult;
			else if (scope === 'input') renderer = ToolInputResult;
			else renderer = ToolSystemResult;
			contractName =
				scope === 'process' ||
				scope === 'window' ||
				scope === 'clipboard' ||
				scope === 'input'
					? scope
					: 'system';
		} else if (adminRendererNames.has(selectedRenderer)) {
			renderer = ToolAdminResult;
			contractName = 'admin';
		} else if (selectedRenderer === 'http') renderer = ToolHttpResult;
		else if (selectedRenderer === 'web_search') renderer = ToolWebSearchResult;
		else if (selectedRenderer === 'memory') renderer = ToolMemoryResult;
		else if (selectedRenderer === 'media') renderer = ToolMediaResult;

		// Extension renderers (notably MCP and Skill) remain open JSON and use
		// JsonView. Builtin components receive their specialized view only when
		// the fields that component consumes have the expected nested shapes.
		if (!renderer) return ToolJsonResult;
		return isValidBuiltinToolResultData(contractName, _data) ? renderer : ToolJsonResult;
	}
	if (kind === 'shell') {
		if (_data == null || isValidBuiltinToolResultData('shell', _data)) return ToolShellResult;
		return ToolJsonResult;
	}
	return kind ? (renderers[kind as keyof typeof renderers] ?? null) : null;
}
