import ToolFileResult from './ToolFileResult.svelte';
import ToolJsonResult from './ToolJsonResult.svelte';
import ToolNotifyResult from './ToolNotifyResult.svelte';
import ToolShellResult from './ToolShellResult.svelte';
import ToolSystemResult from './ToolSystemResult.svelte';

const renderers = {
	shell: ToolShellResult,
	notify: ToolNotifyResult,
	generic: ToolJsonResult,
	raw: ToolJsonResult,
} as const;

/**
 * Resolve the body component for a normalized tool-result kind. Custom tool
 * renderers remain in ToolResultCard until their data-specific state and
 * actions are extracted; this registry is the extension point that keeps
 * simple result types out of the card shell.
 */

export function getToolResultRenderer(
	kind: string | null | undefined,
	toolName = '',
	_data: unknown = null,
) {
	if (kind === 'custom' && toolName === 'file') return ToolFileResult;
	if (kind === 'custom' && toolName === 'system') return ToolSystemResult;
	return kind ? renderers[kind as keyof typeof renderers] ?? null : null;
}
