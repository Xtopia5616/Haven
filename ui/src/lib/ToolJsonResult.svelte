<script lang="ts">
	import JsonView from '$lib/JsonView.svelte';
	import type { ParsedToolResult } from './toolResultParsing.ts';

	interface Props {
		kind?: ParsedToolResult['kind'];
		data?: unknown;
		rawText?: string;
	}

	let { kind = 'generic', data = {}, rawText = '' }: Props = $props();
</script>

{#if kind === 'raw'}
	<pre class="tool-result-preview">{rawText}</pre>
{:else}
	<JsonView value={data} />
{/if}

<style>
</style>
