<script>
	/** A compact explanation of the responsibility of the current workspace. */
	let { title = '', message = '' } = $props();
</script>

<aside class="workspace-scope-note" role="note">
	<span class="workspace-scope-note__mark" aria-hidden="true"></span>
	<div class="workspace-scope-note__copy">
		<strong>{title}</strong>
		<p>{message}</p>
	</div>
</aside>

<style>
	.workspace-scope-note {
		display: flex;
		align-items: flex-start;
		gap: var(--md-sys-space-md);
		margin-bottom: var(--md-sys-space-xl);
		padding: var(--md-sys-space-md) var(--md-sys-space-lg);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-left: 3px solid var(--md-sys-color-primary);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-surface-container-low);
	}
	.workspace-scope-note__mark {
		width: 10px;
		height: 10px;
		margin-top: var(--md-sys-space-xs);
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-primary);
		flex: 0 0 auto;
	}
	.workspace-scope-note__copy {
		min-width: 0;
	}
	.workspace-scope-note strong {
		display: block;
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.workspace-scope-note p {
		margin: var(--md-sys-space-xs) 0 0;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		overflow-wrap: anywhere;
	}
</style>
