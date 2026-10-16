<script lang="ts">
	import JsonView from '$lib/JsonView.svelte';
	import SandboxOutputReviewPanel from '$lib/SandboxOutputReviewPanel.svelte';

	interface Props {
		data: Record<string, unknown>;
		rawText?: string;
		kind?: string | null;
		liveStreaming?: boolean;
		sessionId?: string | null;
		reviewStepIds?: string[];
		failed?: boolean;
	}

	let {
		data,
		rawText = '',
		kind = null,
		liveStreaming = false,
		sessionId = null,
		reviewStepIds = [],
		failed = false,
	}: Props = $props();
	let hasOutput = $derived(Object.keys(data).length > 0 || rawText.length > 0);
</script>

{#if liveStreaming}
	<p class="tool-result-message">子调用正在运行…</p>
{:else if failed && !hasOutput}
	<p class="tool-result-message tool-result-message--error">调用失败</p>
{:else if hasOutput}
	<div class="tool-nested-output">
		<JsonView value={kind === 'raw' ? rawText : data} defaultDepth={1} />
	</div>
{:else}
	<p class="tool-result-message">（无输出）</p>
{/if}

{#if !liveStreaming && sessionId}
	{#each reviewStepIds as sourceStepId (sourceStepId)}
		<SandboxOutputReviewPanel {sessionId} {sourceStepId} />
	{/each}
{/if}

<style>
	.tool-nested-output {
		max-height: 360px;
		overflow: auto;
	}
</style>
