<script>
	let { data = {} } = $props();
</script>

<div class="tool-card-count">{data.count ?? data.windows.length} 个窗口</div>
{#if data.windows.length > 0}
	<div class="tool-card-list">
		{#each data.windows as window (window.hwnd ?? window.title)}
			<div class="window-row">
				<span class="window-title" title={window.title}>{window.title || '(无标题)'}</span>
				{#if window.pid}<span class="window-pid">PID {window.pid}</span>{/if}
			</div>
		{/each}
	</div>
{:else}
	<p class="tool-card-empty">没有可见窗口</p>
{/if}

<style>
	.tool-card-count {
		font-size: 11px;
		font-weight: 600;
		color: var(--md-sys-color-on-surface-variant);
		margin-bottom: var(--md-sys-space-xs);
	}
	.tool-card-list {
		max-height: 200px;
		overflow-y: auto;
		border-radius: var(--md-sys-shape-extra-small);
	}
	.tool-card-empty {
		margin: 0;
		font-size: 12px;
		color: var(--md-sys-color-on-surface-variant);
	}
	.window-row {
		display: flex;
		align-items: baseline;
		gap: var(--md-sys-space-xs);
		padding: 3px var(--md-sys-space-2xs);
		border-radius: 4px;
		font-size: 12px;
	}
	.window-row:nth-child(odd) {
		background: color-mix(in srgb, var(--md-sys-color-on-surface) 4%, transparent);
	}
	.window-title {
		flex: 1;
		min-width: 0;
		font-family: var(--md-sys-typescale-mono);
		font-size: 11px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--md-sys-color-on-surface);
	}
	.window-pid {
		flex: none;
		font-size: 10px;
		color: var(--md-sys-color-on-surface-variant);
	}
</style>
