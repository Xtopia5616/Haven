<script lang="ts">
	import ToolResultCard from '$lib/ToolResultCard.svelte';
	import type { ToolRunPayload } from '$lib/contracts/toolRun.ts';

	interface Props {
		toolRun?: ToolRunPayload;
		awaitingBackgroundResult?: boolean;
		awaitingBackgroundCount?: number;
		showTerminalOutput?: boolean;
	}

	let {
		toolRun = undefined,
		awaitingBackgroundResult = false,
		awaitingBackgroundCount = 0,
		showTerminalOutput = true,
	}: Props = $props();

	const backgroundResult = $derived(
		JSON.stringify({
			tool_run_id: toolRun?.toolRunId,
			tool_name: toolRun?.toolName,
			status: toolRun?.status ?? 'running',
			output: showTerminalOutput
				? toolRun?.output || toolRun?.preview || toolRun?.error || ''
				: '',
		}),
	);
</script>

	<ToolResultCard
		type="tool"
		toolName="toolrun.start"
		outcome={toolRun?.status ?? 'running'}
		content={backgroundResult}
		toolArgs={null}
		messageId={toolRun?.sourceStepId ?? toolRun?.toolRunId ?? 'background-tool-run-wait'}
		toolRunId={showTerminalOutput ? (toolRun?.toolRunId ?? null) : null}
		toolRunData={showTerminalOutput ? (toolRun ?? null) : null}
		toolRunOutputHidden={!!toolRun && !showTerminalOutput}
		{awaitingBackgroundResult}
		{awaitingBackgroundCount}
	/>
