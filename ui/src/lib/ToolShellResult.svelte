<script lang="ts">
	interface Props {
		data?: {
			truncated?: boolean | null;
			exit_code?: number | null;
			sandbox_notice?: string | null;
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
{#if data.sandbox_notice}
	<p class="tool-result-message sandbox-notice" role="note">{data.sandbox_notice}</p>
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
	.sandbox-notice {
		padding: 8px 10px;
		border: 1px solid var(--md-sys-color-warning);
		border-radius: var(--md-sys-shape-corner-small);
		background: var(--md-sys-color-warning-container);
		color: var(--md-sys-color-on-warning-container);
	}

	.tool-result-preview.streaming {
		max-height: 280px;
	}
</style>
