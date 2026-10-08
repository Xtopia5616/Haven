<script lang="ts">
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

<input
	class="md-input md-input--compact tool-search {className}"
	type="search"
	{value}
	{placeholder}
	aria-label={ariaLabel}
	oninput={(event) => onInput(event.currentTarget.value)}
/>

<style>
	.tool-search {
		margin-bottom: var(--md-sys-space-xs);
	}
</style>
