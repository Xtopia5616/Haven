<script>
	import VoiceBars from './VoiceBars.svelte';

	/**
	 * Shared loading surface for workspace views and the conversation shell.
	 * The three bars are the small voice pattern from Haven's mark.
	 * @prop {string} label — accessible loading message
	 * @prop {string} detail — optional accessible supporting message
	 * @prop {'page'|'inline'} variant — content-area or compact inline layout
	 */
	let { label = '正在加载…', detail = '', variant = 'page' } = $props();
	let accessibleLabel = $derived(detail ? `${label}，${detail}` : label);
</script>

<div
	class="loading-state loading-state--{variant}"
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
	/* Page-level loaders cover the workspace content only. Keeping the chrome
	 * outside this layer lets the titlebar and workspace navigation remain
	 * visible and usable while the first view or a lazy view is loading. */
	.loading-state--page {
		position: fixed;
		inset: calc(var(--md-comp-titlebar-height) + var(--md-comp-tab-container-height)) 0 0;
		box-sizing: border-box;
		z-index: var(--md-sys-z-drawer);
		min-height: 0;
		padding: 0;
		pointer-events: none;
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
