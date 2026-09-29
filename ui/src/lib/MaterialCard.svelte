<script lang="ts">
	import type { Snippet } from 'svelte';

	interface Props {
		variant?: 'filled' | 'outlined' | 'elevated';
		className?: string;
		children?: Snippet;
	}

	/**
	 * Material Card — shared surface container for grouped content.
	 */
	let { variant = 'filled', className = '', children }: Props = $props();
</script>

<div class="md-card md-card--{variant} {className}">
	{@render children?.()}
</div>
