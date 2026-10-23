<script lang="ts">
	import { getMarkdownRenderer, renderMarkdown } from '$lib/markdownRenderer.ts';
	import { copyText } from '$lib/clipboard.ts';
	import { handleExtRefEvent } from '$lib/externalRef.ts';

	let { markdown }: { markdown: string } = $props();
	let html = $state('');

	function handleMarkdownClick(event: MouseEvent) {
		if (handleExtRefEvent(event)) return;
		const target = event.target instanceof Element ? event.target : null;
		const button = target?.closest('.md-code-copy');
		const code = button?.closest('.md-code-wrap')?.querySelector('code')?.textContent;
		if (code == null) return;
		event.preventDefault();
		event.stopPropagation();
		void copyText(code, '代码');
	}

	function handleMarkdownContextMenu(event: MouseEvent) {
		handleExtRefEvent(event);
	}

	function markdownInteractions(node: HTMLElement) {
		node.addEventListener('click', handleMarkdownClick);
		node.addEventListener('contextmenu', handleMarkdownContextMenu);
		return {
			destroy() {
				node.removeEventListener('click', handleMarkdownClick);
				node.removeEventListener('contextmenu', handleMarkdownContextMenu);
			},
		};
	}

	$effect(() => {
		const source = markdown;
		let current = true;
		void getMarkdownRenderer().then(() => {
			if (current) html = renderMarkdown(source);
		});
		return () => {
			current = false;
		};
	});
</script>

<section class="working-document" aria-label="会话工作文档">
	<div class="working-document-heading">
		<span class="material-symbol" aria-hidden="true">description</span>
		<h2>工作文档</h2>
		<span class="working-document-hint">Agent 的会话内目标、计划与进展</span>
	</div>
	<div
		class="working-document-content md-content"
		use:markdownInteractions
	>
		{@html html}
	</div>
</section>

<style>
	.working-document {
		flex: 0 0 auto;
		max-height: min(32vh, 300px);
		margin: var(--md-sys-space-md) auto var(--md-sys-space-sm);
		width: min(100%, 820px);
		overflow: auto;
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-large);
		background: color-mix(in srgb, var(--md-sys-color-surface-container) 86%, transparent);
		padding: var(--md-sys-space-md) var(--md-sys-space-lg);
	}

	.working-document-heading {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		margin-bottom: var(--md-sys-space-sm);
		color: var(--md-sys-color-on-surface-variant);
	}

	.working-document-heading h2 {
		margin: 0;
		font-size: var(--md-sys-typescale-body-small-size);
		font-weight: 650;
		line-height: var(--md-sys-typescale-body-small-line-height);
		color: var(--md-sys-color-on-surface);
	}

	.material-symbol {
		font-family: 'Material Symbols Rounded';
		font-size: var(--md-sys-icon-size);
		font-weight: normal;
		font-style: normal;
		line-height: 1;
	}

	.working-document-hint {
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		opacity: 0.75;
	}

	.working-document-content {
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
		color: var(--md-sys-color-on-surface);
		overflow-wrap: anywhere;
	}

	.working-document-content :global(:first-child) {
		margin-top: 0;
	}

	.working-document-content :global(:last-child) {
		margin-bottom: 0;
	}

	.working-document-content :global(h1),
	.working-document-content :global(h2),
	.working-document-content :global(h3) {
		font-size: 1.05em;
		margin: 0.8em 0 0.35em;
	}

	.working-document-content :global(ul),
	.working-document-content :global(ol) {
		padding-left: 1.4em;
	}

	.working-document-content :global(pre),
	.working-document-content :global(.md-code-wrap),
	.working-document-content :global(blockquote) {
		max-width: 100%;
		overflow-x: auto;
	}

	.working-document-content :global(blockquote) {
		margin-left: 0;
		padding-left: var(--md-sys-space-md);
		border-left: 2px solid var(--md-sys-color-outline-variant);
		color: var(--md-sys-color-on-surface-variant);
	}
</style>
