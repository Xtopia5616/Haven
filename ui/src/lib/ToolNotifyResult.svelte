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
		font-size: 12px;
		font-weight: 700;
		color: var(--md-sys-color-on-surface);
	}
	.notify-body {
		margin: 0;
		font-size: 12px;
		line-height: 1.5;
		color: var(--md-sys-color-on-surface-variant);
	}
</style>
