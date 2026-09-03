<script>
	import Logo from './Logo.svelte';

	/**
	 * Shared loading surface for workspace views and the conversation shell.
	 * The animated voice bars echo Haven's mark without adding visual noise.
	 * @prop {string} label — primary loading message
	 * @prop {string} detail — optional supporting message
	 * @prop {'page'|'inline'} variant — full-page or compact inline layout
	 */
	let { label = '正在加载…', detail = '', variant = 'page' } = $props();
</script>

<div
	class="loading-state loading-state--{variant}"
	role="status"
	aria-live="polite"
	aria-busy="true"
>
	<div class="loading-state__visual" aria-hidden="true">
		<span class="loading-state__halo"></span>
		<span class="loading-state__mark"><Logo size={34} /></span>
		<span class="loading-state__bars">
			<span class="loading-state__bar loading-state__bar--short"></span>
			<span class="loading-state__bar loading-state__bar--tall"></span>
			<span class="loading-state__bar loading-state__bar--medium"></span>
		</span>
	</div>
	<div class="loading-state__copy">
		<span class="loading-state__label">{label}</span>
		{#if detail}<span class="loading-state__detail">{detail}</span>{/if}
	</div>
</div>

<style>
	.loading-state {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: var(--md-sys-space-md);
		min-height: calc(var(--md-sys-space-4xl) * 5);
		padding: var(--md-sys-space-4xl) var(--md-sys-space-2xl);
		color: var(--md-sys-color-on-surface-variant);
		text-align: center;
	}
	.loading-state--inline {
		flex-direction: row;
		gap: var(--md-sys-space-sm);
		min-height: var(--md-sys-space-4xl);
		padding: var(--md-sys-space-2xl) var(--md-sys-space-md);
	}
	.loading-state__visual {
		position: relative;
		display: grid;
		place-items: center;
		width: 80px;
		height: 72px;
		flex-shrink: 0;
	}
	.loading-state--inline .loading-state__visual {
		transform: scale(0.72);
		transform-origin: center;
		margin: -10px -8px;
	}
	.loading-state__halo {
		position: absolute;
		width: 64px;
		height: 64px;
		border: 1px solid color-mix(in srgb, var(--md-sys-color-primary) 26%, transparent);
		border-radius: 22px;
		animation: loading-halo 1.8s var(--md-sys-motion-easing-emphasized) infinite;
	}
	.loading-state__mark {
		position: relative;
		z-index: 1;
		display: grid;
		place-items: center;
		width: 52px;
		height: 52px;
		border: 1px solid color-mix(in srgb, var(--md-sys-color-primary) 20%, transparent);
		border-radius: 18px;
		background: var(--md-sys-color-surface-container-high);
		box-shadow: var(--md-sys-elevation-1);
		animation: loading-mark 1.8s var(--md-sys-motion-easing-emphasized) infinite;
	}
	.loading-state__bars {
		position: absolute;
		bottom: 1px;
		z-index: 2;
		display: inline-flex;
		align-items: flex-end;
		gap: 3px;
		padding: 4px 6px;
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-surface-container);
		box-shadow: 0 2px 6px color-mix(in srgb, var(--md-sys-color-shadow, #000) 18%, transparent);
	}
	.loading-state__bar {
		display: block;
		width: 4px;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-primary);
		transform-origin: bottom;
		animation: loading-bar 1.1s var(--md-sys-motion-easing-emphasized) infinite;
	}
	.loading-state__bar--short {
		height: 8px;
		animation-delay: -0.28s;
	}
	.loading-state__bar--tall {
		height: 14px;
		animation-delay: -0.08s;
	}
	.loading-state__bar--medium {
		height: 10px;
		animation-delay: 0.12s;
	}
	.loading-state__copy {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 2px;
	}
	.loading-state--inline .loading-state__copy {
		align-items: flex-start;
		text-align: left;
	}
	.loading-state__label {
		font-size: var(--md-sys-typescale-title-medium-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-title-medium-line-height);
		color: var(--md-sys-color-on-surface);
	}
	.loading-state--inline .loading-state__label {
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.loading-state__detail {
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	@keyframes loading-halo {
		0%,
		100% {
			opacity: 0.45;
			transform: scale(0.92);
		}
		50% {
			opacity: 1;
			transform: scale(1.02);
		}
	}
	@keyframes loading-mark {
		0%,
		100% {
			transform: translateY(1px);
		}
		50% {
			transform: translateY(-1px);
		}
	}
	@keyframes loading-bar {
		0%,
		100% {
			opacity: 0.5;
			transform: scaleY(0.58);
		}
		50% {
			opacity: 1;
			transform: scaleY(1);
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.loading-state__halo,
		.loading-state__mark,
		.loading-state__bar {
			animation: none;
		}
		.loading-state__halo,
		.loading-state__mark {
			opacity: 0.8;
		}
	}
	@media (max-width: 455px) {
		.loading-state {
			padding-inline: var(--md-sys-space-lg);
		}
	}
</style>
