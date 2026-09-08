<script>
	/**
	 * Settings Field — shared label/control row for settings forms.
	 * @prop {string} label
	 * @prop {string | undefined} id
	 * @prop {string} description
	 * @prop {boolean} stacked
	 * @prop {string} className
	 */
	let {
		label = '',
		id = undefined,
		description = '',
		stacked = false,
		className = '',
		children,
	} = $props();
</script>

<div class="settings-field {className}" class:settings-field--stacked={stacked}>
	<div class="settings-field__label">
		{#if id}<label for={id}>{label}</label>{:else}<span>{label}</span>{/if}
		{#if description}<small>{description}</small>{/if}
	</div>
	<div class="settings-field__control">{@render children?.()}</div>
</div>

<style>
	.settings-field {
		display: grid;
		grid-template-columns: minmax(180px, 0.8fr) minmax(220px, 1.2fr);
		align-items: center;
		gap: var(--md-sys-space-lg);
		min-height: var(--md-comp-list-item-one-line-height);
		padding-block: var(--md-sys-space-sm);
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
	}

	.settings-field:last-child {
		border-bottom: 0;
	}

	.settings-field__label {
		min-width: 0;
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}

	.settings-field__label label,
	.settings-field__label span {
		display: block;
	}

	.settings-field__label small {
		display: block;
		margin-top: var(--md-sys-space-2xs);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}

	.settings-field__control {
		min-width: 0;
	}

	.settings-field--stacked {
		grid-template-columns: 1fr;
		align-items: start;
	}

	@media (max-width: 640px) {
		.settings-field {
			grid-template-columns: 1fr;
			gap: var(--md-sys-space-xs);
			align-items: start;
		}
	}
</style>
