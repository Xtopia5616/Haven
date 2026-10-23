<script lang="ts">
	import WorkspaceSearch from './WorkspaceSearch.svelte';
	interface Props {
		value?: string;
		onInput?: (value: string) => void;
		placeholder?: string;
		ariaLabel?: string;
		className?: string;
	}

	/**
	 * Tool Search — shared compact search field for structured tool results.
	 */
	let {
		value = '',
		onInput = () => {},
		placeholder = '筛选…',
		ariaLabel = '筛选',
		className = '',
	}: Props = $props();
</script>

<div class="tool-search {className}">
	<WorkspaceSearch {value} {placeholder} label={ariaLabel} {onInput} compact />
</div>

<style>
	.tool-search {
		margin-bottom: var(--md-sys-space-xs);
	}
</style>
