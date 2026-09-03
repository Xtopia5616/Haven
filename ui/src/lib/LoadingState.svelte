<script>
	/**
	 * Shared loading surface for workspace views and the conversation shell.
	 * The three bars are the small voice pattern from Haven's mark.
	 * @prop {string} label — accessible loading message
	 * @prop {string} detail — optional accessible supporting message
	 * @prop {'page'|'inline'} variant — full-page or compact inline layout
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
	<div class="loading-state__visual" aria-hidden="true">
		<span class="loading-state__bar loading-state__bar--short"></span>
		<span class="loading-state__bar loading-state__bar--tall"></span>
		<span class="loading-state__bar loading-state__bar--medium"></span>
	</div>
</div>

<style>
	.loading-state {
		display: grid;
		place-items: center;
		min-height: calc(var(--md-sys-space-4xl) * 5);
		padding: var(--md-sys-space-4xl) var(--md-sys-space-2xl);
	}
	.loading-state--inline {
		min-height: var(--md-sys-space-4xl);
		padding: var(--md-sys-space-2xl) var(--md-sys-space-md);
	}
	.loading-state__visual {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		height: 32px;
	}
	.loading-state__bar {
		display: block;
		width: 5px;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-primary);
		animation: loading-float 1.2s ease-in-out infinite;
	}
	.loading-state__bar--short {
		height: 14px;
		animation-delay: -0.16s;
	}
	.loading-state__bar--tall {
		height: 24px;
		animation-delay: 0s;
	}
	.loading-state__bar--medium {
		height: 18px;
		animation-delay: 0.16s;
	}
	@keyframes loading-float {
		0%,
		100% {
			opacity: 0.5;
			transform: translateY(4px);
		}
		50% {
			opacity: 1;
			transform: translateY(-4px);
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.loading-state__bar {
			animation: none;
			opacity: 0.8;
		}
	}
	@media (max-width: 455px) {
		.loading-state {
			padding-inline: var(--md-sys-space-lg);
		}
	}
</style>
