<script>
	let { parts = { title: '', body: '' } } = $props();
</script>

{#if parts.title}
	<p class="notify-title">{parts.title}</p>
{/if}
{#if parts.body}
	<p class="notify-body">{parts.body}</p>
{/if}

<style>
	.notify-title {
		margin: 0 0 var(--md-sys-space-2xs);
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface);
	}
	.notify-body {
		margin: 0;
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
</style>
