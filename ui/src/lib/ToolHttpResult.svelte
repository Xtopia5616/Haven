<script lang="ts">
	import StatusBadge from '$lib/StatusBadge.svelte';

	interface Props {
		data?: {
			status?: number;
			truncated?: boolean;
			body?: string | null;
		};
	}

	let { data = {} }: Props = $props();
</script>

<div class="tool-result-status-row">
	<StatusBadge
		label={String(data.status)}
		tone={Number(data.status) >= 200 && Number(data.status) < 300 ? 'success' : 'error'}
		className={Number(data.status) >= 200 && Number(data.status) < 300
			? 'status-completed'
			: 'status-failed'}
	/>
	{#if data.truncated}<span class="tool-result-meta tool-result-meta--compact"
			>（响应过长已截断）</span
		>{/if}
</div>
{#if typeof data.body === 'string' && data.body}
	<pre class="tool-result-preview">{data.body}</pre>
{/if}
