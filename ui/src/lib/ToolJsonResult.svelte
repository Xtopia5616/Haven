<script lang="ts">
	import JsonView from '$lib/JsonView.svelte';

	interface Props {
		kind?: string;
		data?: unknown;
		rawText?: string;
	}

	let { kind = 'generic', data = {}, rawText = '' }: Props = $props();
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
		font-size: var(--md-sys-typescale-code-size);
		line-height: var(--md-sys-typescale-code-line-height);
		white-space: pre-wrap;
		word-break: break-word;
		max-height: 180px;
		overflow-y: auto;
		margin: var(--md-sys-space-xs) 0 0;
	}
</style>
