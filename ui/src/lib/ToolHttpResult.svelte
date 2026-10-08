<script lang="ts">
	import StatusBadge from '$lib/StatusBadge.svelte';

	interface Props {
		data?: {
			status?: number | null;
			truncated?: boolean | null;
			body?: string | null;
		};
	}

	let { data = {} }: Props = $props();
</script>

{#if (typeof data.status === 'number' && Number.isFinite(data.status)) || data.truncated}
	<div class="tool-result-status-row">
		{#if typeof data.status === 'number' && Number.isFinite(data.status)}
			<StatusBadge
				label={String(data.status)}
				tone={Number(data.status) >= 200 && Number(data.status) < 300 ? 'success' : 'error'}
				className={Number(data.status) >= 200 && Number(data.status) < 300
					? 'status-completed'
					: 'status-failed'}
			/>
		{/if}
		{#if data.truncated}
			<span class="tool-result-meta tool-result-meta--compact">（响应过长已截断）</span>
		{/if}
	</div>
{/if}
{#if typeof data.body === 'string' && data.body}
	<pre class="tool-result-preview">{data.body}</pre>
{/if}
