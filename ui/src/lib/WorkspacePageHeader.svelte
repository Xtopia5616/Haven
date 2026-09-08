<script>
	/**
	 * WorkspacePageHeader — shared page title rhythm for secondary workspaces.
	 * @prop {string} title — page title
	 * @prop {string} description — short explanation of the workspace
	 * @prop {string | undefined} headingId — optional id for the page heading
	 * @prop {import('svelte').Snippet | undefined} children — optional page actions
	 */
	let { title = '', description = '', headingId = undefined, children = undefined } = $props();
</script>

<header class="workspace-page-header page-heading">
	<div class="page-heading-content">
		<h1 id={headingId}>{title}</h1>
		{#if description}<p>{description}</p>{/if}
	</div>
	{#if children}
		<div class="page-heading-actions">
			{@render children?.()}
		</div>
	{/if}
</header>
