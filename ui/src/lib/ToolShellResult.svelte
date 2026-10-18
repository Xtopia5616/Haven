<script lang="ts">
	import SandboxOutputReviewPanel from './SandboxOutputReviewPanel.svelte';

	interface Props {
		data?: {
			truncated?: boolean | null;
			exit_code?: number | null;
		};
		shellText?: string;
		liveStreaming?: boolean;
		sessionId?: string | null;
		sourceStepId?: string | null;
		failed?: boolean;
	}

	let {
		data = {},
		shellText = '',
		liveStreaming = false,
		sessionId = null,
		sourceStepId = null,
		failed = false,
	}: Props = $props();
	let showReview = $derived(
		!!sessionId &&
			!!sourceStepId &&
			!liveStreaming,
	);
</script>

{#if data.truncated}
	<div class="tool-result-label">输出过长已截断</div>
{/if}
{#if data.exit_code != null}
	<div class="tool-result-meta tool-result-meta--shell">退出码 {data.exit_code}</div>
{/if}
{#if shellText}
	<pre class="tool-result-preview" class:streaming={liveStreaming}>{shellText}</pre>
{:else if liveStreaming}
	<p class="tool-result-message">等待输出…</p>
{:else if failed}
	<p class="tool-result-message tool-result-message--error">调用失败</p>
{:else}
	<p class="tool-result-message">（无输出）</p>
{/if}
{#if showReview && sessionId && sourceStepId}
	<SandboxOutputReviewPanel {sessionId} {sourceStepId} />
{/if}

<style>
	.tool-result-preview.streaming {
		max-height: 280px;
	}
</style>
