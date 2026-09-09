<script>
	/**
	 * WorkspaceSectionHeader — shared section title rhythm inside secondary tabs.
	 * @prop {string} title — section title
	 * @prop {string} description — short explanation of the section
	 * @prop {string | undefined} headingId — optional id for the section heading
	 * @prop {import('svelte').Snippet | undefined} children — optional section actions
	 */
	let { title = '', description = '', headingId = undefined, children = undefined } = $props();
</script>

<header class="workspace-section-header">
	<div class="workspace-section-header__content">
		<h2 id={headingId}>{title}</h2>
		{#if description}<p>{description}</p>{/if}
	</div>
	{#if children}
		<div class="workspace-section-header__actions">
			{@render children?.()}
		</div>
	{/if}
</header>

<style>
	.workspace-section-header {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
		min-width: 0;
		margin-bottom: var(--md-sys-space-lg);
	}
	.workspace-section-header__content {
		flex: 1 1 0;
		min-width: 0;
	}
	.workspace-section-header h2 {
		margin: 0;
		font-size: var(--md-sys-typescale-title-large-size);
		font-weight: 650;
		line-height: var(--md-sys-typescale-title-large-line-height);
		color: var(--md-sys-color-on-surface);
	}
	.workspace-section-header p {
		margin: var(--md-sys-space-xs) 0 0;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.workspace-section-header__actions {
		display: flex;
		align-items: center;
		justify-content: flex-end;
		flex: 0 0 auto;
		flex-wrap: wrap;
		gap: var(--md-sys-space-sm);
	}
	@media (max-width: 700px) {
		.workspace-section-header {
			align-items: flex-start;
			flex-direction: column;
		}
		.workspace-section-header__actions {
			width: 100%;
			justify-content: flex-start;
		}
	}
</style>
