<script>
	/**
	 * Haven — unified app logo. A rounded "speech bubble + soundwave" glyph
	 * whose colors follow the M3 token system so it adapts to light/dark.
	 * Props:
	 *   size  — px (default 24)
	 *   mark  — boolean, render bare glyph without wordmark (default true)
	 */
	let { size = 24, withText = false } = $props();
</script>

<span class="logo" class:with-text={withText} style="--logo-size: {size}px;">
	<span class="glyph" aria-hidden="true">
		<svg viewBox="0 0 24 24" width={size} height={size} fill="none">
			<path
				d="M4 6.5C4 5.12 5.12 4 6.5 4h11C18.88 4 20 5.12 20 6.5v7c0 1.38-1.12 2.5-2.5 2.5H11l-4 3.5V16H6.5C5.12 16 4 14.88 4 13.5v-7Z"
				fill="var(--md-sys-color-primary-container)"
			/>
			<g stroke="var(--md-sys-color-on-primary-container)" stroke-width="2" stroke-linecap="round">
				<line x1="9" y1="10" x2="9" y2="11" />
				<line x1="12" y1="9" x2="12" y2="12" />
				<line x1="15" y1="10" x2="15" y2="11" />
			</g>
		</svg>
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