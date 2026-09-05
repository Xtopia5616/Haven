<script>
	/**
	 * Shared form-field layout for a label, control, hint, and validation error.
	 * @prop {string} label — visible field label
	 * @prop {string | undefined} forId — id of the associated native control
	 * @prop {string} hint — optional supporting text
	 * @prop {string} error — optional validation message
	 * @prop {import('svelte').Snippet} children — field control content
	 */
	let { label = '', forId = undefined, hint = '', error = '', children } = $props();
</script>

<div class="md-field">
	{#if label}
		<label class="md-field__label" for={forId}>{label}</label>
	{/if}
	{@render children?.()}
	{#if hint}
		<span class="md-field__hint">{hint}</span>
	{/if}
	{#if error}
		<span class="md-field__error" role="alert">{error}</span>
	{/if}
</div>

<style>
	.md-field {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
	}
	.md-field__label {
		font-size: var(--md-sys-typescale-label-medium-size);
		color: var(--md-sys-color-on-surface-variant);
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: var(--md-sys-typescale-overline-letter-spacing);
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.md-field__hint,
	.md-field__error {
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.md-field__hint {
		color: var(--md-sys-color-on-surface-variant);
	}
	.md-field__error {
		color: var(--md-sys-color-error);
	}
</style>
