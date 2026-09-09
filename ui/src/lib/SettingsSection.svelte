<script>
	/**
	 * Settings Section — consistent grouped settings surface and heading rhythm.
	 * @prop {string} title
	 * @prop {string} description
	 * @prop {string} className
	 * @prop {string | undefined} ariaLabel — accessible name when the visible title is omitted
	 */
	let {
		title = '',
		description = '',
		className = '',
		ariaLabel = undefined,
		children,
		header = undefined,
	} = $props();
</script>

<section class="settings-section {className}" aria-label={ariaLabel}>
	{#if header}
		<div class="settings-section__header">{@render header()}</div>
	{:else if title}
		<h2>{title}</h2>
	{/if}
	{#if description}<p class="settings-section__description">{description}</p>{/if}
	{@render children?.()}
</section>

<style>
	.settings-section {
		background: var(--md-sys-color-surface-container-low);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-large);
		padding: var(--md-sys-space-xl);
		margin-bottom: var(--md-sys-space-xl);
	}

	.settings-section h2 {
		margin: 0 0 var(--md-sys-space-lg);
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-title-medium-size);
		font-weight: 700;
		letter-spacing: 0;
		line-height: var(--md-sys-typescale-title-medium-line-height);
	}

	.settings-section__header {
		margin-bottom: var(--md-sys-space-lg);
	}

	.settings-section__description {
		margin: 0 0 var(--md-sys-space-md);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
</style>
