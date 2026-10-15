<script lang="ts">
	import SandboxOutputReviewPanel from './SandboxOutputReviewPanel.svelte';
	import type { ToolRunStatus } from '$lib/contracts/toolRun.ts';
	import type { ToolExecutionMode } from './toolResultPresentation.ts';

	interface Props {
		data?: {
			truncated?: boolean | null;
			execution_mode?: ToolExecutionMode | null;
			status?: ToolRunStatus | null;
			tool_run_id?: string | null;
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
			data.execution_mode !== 'background' &&
			data.status !== 'running' &&
			!liveStreaming,
	);
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
		<div class="tool-result-meta tool-result-meta--shell">退出码 {data.exit_code}</div>
	{/if}
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
