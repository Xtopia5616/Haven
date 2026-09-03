<script>
	import HavenMark from './HavenMark.svelte';

	/**
	 * Haven — unified app logo. A rounded conversation bubble with three thick
	 * voice bars, matching assets/branding/haven-mark.svg.
	 * Props:
	 *   size  — px (default 24)
	 *   withText  — render the Haven wordmark beside the mark
	 */
	let { size = 24, withText = false } = $props();
</script>

<span class="logo" class:with-text={withText} style="--logo-size: {size}px;">
		<span class="glyph">
			<HavenMark size={size} />
	</span>
	{#if withText}
		<span class="wordmark">Haven</span>
	{/if}
</span>

<style>
	.logo {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		line-height: 1;
	}
	.glyph {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: var(--logo-size);
		height: var(--logo-size);
	}
	.wordmark {
		font-family: var(--md-ref-typeface-brand);
		font-size: calc(var(--logo-size) * 0.72);
		font-weight: 700;
		letter-spacing: 0.4px;
		color: var(--md-sys-color-primary);
	}
</style>
