<script>
	import JsonView from '$lib/JsonView.svelte';

	let { kind = 'generic', data = {}, rawText = '' } = $props();
</script>

{#if kind === 'raw'}
	<pre class="content-preview">{rawText}</pre>
{:else}
	<JsonView value={data} />
{/if}

<style>
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
</style>
