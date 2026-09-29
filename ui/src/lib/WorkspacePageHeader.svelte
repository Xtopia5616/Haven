<script lang="ts">
	import type { Snippet } from 'svelte';

	interface Props {
		title?: string;
		description?: string;
		headingId?: string;
		children?: Snippet;
	}

	/**
	 * WorkspacePageHeader — shared page title rhythm for secondary workspaces.
	 */
	let {
		title = '',
		description = '',
		headingId = undefined,
		children = undefined,
	}: Props = $props();
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
