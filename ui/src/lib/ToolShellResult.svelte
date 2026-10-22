<script lang="ts">
	interface Props {
		data?: {
			truncated?: boolean | null;
			exit_code?: number | null;
		};
		shellText?: string;
		liveStreaming?: boolean;
		failed?: boolean;
	}

	let {
		data = {},
		shellText = '',
		liveStreaming = false,
		failed = false,
	}: Props = $props();
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
<style>
	.tool-result-preview.streaming {
		max-height: 280px;
	}
</style>
