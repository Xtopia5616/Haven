<script lang="ts">
	import VoiceBars from './VoiceBars.svelte';

	interface Props {
		label?: string;
		detail?: string;
		variant?: 'page' | 'inline';
	}

	/**
	 * Shared loading surface for workspace views and the conversation shell.
	 * The three bars are the small voice pattern from Haven's mark.
	 */
	let { label = '正在加载…', detail = '', variant = 'page' }: Props = $props();
	let accessibleLabel = $derived(detail ? `${label}，${detail}` : label);
</script>

<div
	class="loading-state loading-state--{variant} motion-surface-enter"
	role="status"
	aria-live="polite"
	aria-busy="true"
	aria-label={accessibleLabel}
>
	<VoiceBars pattern="float" count={3} />
</div>

<style>
	.loading-state {
		display: grid;
		place-items: center;
		min-height: calc(var(--md-sys-space-4xl) * 5);
		padding: var(--md-sys-space-4xl) var(--md-sys-space-2xl);
	}
	/* Page-level loading blocks the whole app until the requested workspace is
	 * ready, so the shell and its navigation cannot show stale content or accept
	 * input behind the loading indicator. */
	.loading-state--page {
		position: fixed;
		inset: 0;
		box-sizing: border-box;
		z-index: var(--md-sys-z-drawer);
		min-height: 0;
		padding: 0;
		background: var(--md-sys-color-background);
		pointer-events: auto;
	}
	.loading-state--inline {
		min-height: var(--md-sys-space-4xl);
		padding: var(--md-sys-space-2xl) var(--md-sys-space-md);
	}
	@media (max-width: 455px) {
		.loading-state {
			padding-inline: var(--md-sys-space-lg);
		}
	}
</style>
