<script>
	let { data = {}, shellText = '', liveStreaming = false } = $props();
</script>

{#if data.truncated}
	<div class="tool-card-count">输出过长已截断</div>
{/if}
{#if data.background && data.status === 'running'}
	<div class="tool-card-count">后台运行中{#if data.action_id} · {data.action_id}{/if}</div>
{:else if data.background && data.status === 'cancelled'}
	<div class="tool-card-count">后台已取消{#if data.action_id} · {data.action_id}{/if}</div>
{:else if data.background && (data.status === 'completed' || data.status === 'failed')}
	<div class="tool-card-count">
		后台{data.status === 'completed' ? '已完成' : '失败'}{#if data.action_id}
			· {data.action_id}{/if}
	</div>
{/if}
{#if shellText}
	<pre class="content-preview" class:streaming={liveStreaming}>{shellText}</pre>
{:else if liveStreaming}
	<p class="tool-card-empty">等待输出…</p>
{:else}
	<p class="tool-card-empty">（无输出）</p>
{/if}

<style>
	.tool-card-count {
		font-size: 11px;
		font-weight: 600;
		color: var(--md-sys-color-on-surface-variant);
		margin-bottom: var(--md-sys-space-xs);
	}
	.tool-card-empty {
		margin: 0;
		font-size: 12px;
		color: var(--md-sys-color-on-surface-variant);
	}
	.content-preview {
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		font-family: var(--md-sys-typescale-mono);
		font-size: 11px;
		white-space: pre-wrap;
		word-break: break-word;
		max-height: 180px;
		overflow-y: auto;
		margin: var(--md-sys-space-xs) 0 0;
	}
	.content-preview.streaming {
		max-height: 280px;
	}
</style>
