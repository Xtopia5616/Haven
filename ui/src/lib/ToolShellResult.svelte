<script lang="ts">
	interface Props {
		data?: {
			truncated?: boolean;
			execution_mode?: 'foreground' | 'background';
			status?: string;
			tool_run_id?: string;
			exit_code?: number | null;
		};
		shellText?: string;
		liveStreaming?: boolean;
	}

	let { data = {}, shellText = '', liveStreaming = false }: Props = $props();
</script>

{#if data.truncated}
	<div class="tool-result-label">输出过长已截断</div>
{/if}
{#if data.execution_mode === 'background' && data.status === 'running'}
	<div class="tool-result-label">
		后台任务运行中{#if data.tool_run_id}
			· {data.tool_run_id}{/if}
	</div>
{:else if data.execution_mode === 'background' && data.status === 'cancelled'}
	<div class="tool-result-label">
		后台任务已取消{#if data.tool_run_id}
			· {data.tool_run_id}{/if}
	</div>
{:else if data.execution_mode === 'background' && (data.status === 'completed' || data.status === 'failed')}
	<div class="tool-result-label">
		后台任务{data.status === 'completed' ? '已完成' : '失败'}{#if data.tool_run_id}
			· {data.tool_run_id}{/if}
	</div>
	{#if data.exit_code != null}
		<div class="tool-card-meta">退出码 {data.exit_code}</div>
	{/if}
{/if}
{#if shellText}
	<pre class="tool-result-preview" class:streaming={liveStreaming}>{shellText}</pre>
{:else if liveStreaming}
	<p class="tool-card-empty">等待输出…</p>
{:else}
	<p class="tool-card-empty">（无输出）</p>
{/if}

<style>
	.tool-card-empty,
	.tool-card-meta {
		margin: 0;
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.tool-card-meta {
		margin-top: var(--md-sys-space-2xs);
	}
	.tool-result-preview.streaming {
		max-height: 280px;
	}
</style>
