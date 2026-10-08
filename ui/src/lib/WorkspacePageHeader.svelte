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
		children: actions = undefined,
	}: Props = $props();
</script>

<header class="workspace-page-header page-heading">
	<div class="page-heading-content">
		<h1 id={headingId}>{title}</h1>
		{#if description}<p>{description}</p>{/if}
	</div>
	{#if actions}
		<div class="page-heading-actions md-toolbar" role="toolbar" aria-label={`${title}操作`}>
			{@render actions?.()}
		</div>
	{/if}
</header>
