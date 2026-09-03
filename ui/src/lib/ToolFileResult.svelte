<script>
	import ExternalRef from '$lib/ExternalRef.svelte';

	let { data = {} } = $props();

	/** @param {unknown} value */
	function fmtBytes(value) {
		const n = Number(value);
		if (!Number.isFinite(n) || n < 0) return '—';
		if (n < 1024) return `${n} B`;
		const units = ['KB', 'MB', 'GB', 'TB'];
		let unit = n;
		let index = -1;
		while (unit >= 1024 && index < units.length - 1) {
			unit /= 1024;
			index++;
		}
		return `${unit >= 100 ? unit.toFixed(0) : unit.toFixed(1)} ${units[index]}`;
	}
</script>

{#if data.written}
	<div class="file-row">
		<span class="file-op">已写入</span><ExternalRef class="file-path" target={data.path} />
	</div>
{:else if data.edited}
	<div class="file-row">
		<span class="file-op">已编辑</span><ExternalRef class="file-path" target={data.path} />{#if data.line != null}<span class="file-line">L{data.line}</span>{/if}
	</div>
{:else if data.copied}
	<div class="file-row">
		<span class="file-op">已复制</span><ExternalRef class="file-path" target={data.from} />
	</div>
	<div class="file-row">
		<span class="file-op-to">→</span><ExternalRef class="file-path" target={data.to} />
	</div>
{:else if data.moved}
	<div class="file-row">
		<span class="file-op">已移动</span><ExternalRef class="file-path" target={data.from} />
	</div>
	<div class="file-row">
		<span class="file-op-to">→</span><ExternalRef class="file-path" target={data.to} />
	</div>
{:else if data.deleted}
	<div class="file-row">
		<span class="file-op">已删除</span><ExternalRef class="file-path" target={data.path} />
	</div>
{:else if Array.isArray(data.entries)}
	<div class="tool-card-count">{data.count ?? data.entries.length} 项</div>
	{#if data.entries.length > 0}
		<div class="tool-card-list">
			{#each data.entries as entry (entry)}
				<div class="env-row"><span class="env-name">{entry}</span></div>
			{/each}
		</div>
	{:else}
		<p class="tool-card-empty">（空目录）</p>
	{/if}
{:else}
	<div class="tool-card-meta">
		{data.size != null ? `${fmtBytes(data.size)} · ` : ''}读取完成
	</div>
	{#if typeof data.content === 'string' && data.content}
		<pre class="content-preview">{data.content}</pre>
	{/if}
{/if}

<style>
	.tool-card-count {
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		margin-bottom: var(--md-sys-space-xs);
	}
	.tool-card-meta {
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		margin-top: var(--md-sys-space-2xs);
	}
	.tool-card-empty {
		margin: 0;
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.tool-card-list {
		max-height: 200px;
		overflow-y: auto;
		border-radius: var(--md-sys-shape-extra-small);
	}
	.file-row,
	.env-row {
		display: flex;
		align-items: baseline;
		gap: var(--md-sys-space-xs);
		padding: 3px var(--md-sys-space-2xs);
		border-radius: 4px;
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.env-row:nth-child(odd) {
		background: color-mix(in srgb, var(--md-sys-color-on-surface) 4%, transparent);
	}
	.env-name {
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-code-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-code-line-height);
		color: var(--md-sys-color-secondary);
	}
	.file-op {
		flex: none;
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-label-small-line-height);
		padding: 1px 6px;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
	}
	.file-op-to {
		flex: none;
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		width: 34px;
		text-align: center;
	}
	:global(.file-path) {
		flex: 1;
		min-width: 0;
		color: var(--md-sys-color-primary);
		text-decoration: underline;
		text-underline-offset: 2px;
		cursor: pointer;
	}
	:global(.file-path:hover) {
		color: color-mix(in srgb, var(--md-sys-color-primary) 80%, var(--md-sys-color-on-surface));
	}
	.file-line {
		flex: none;
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-secondary);
	}
	.content-preview {
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-code-size);
		line-height: var(--md-sys-typescale-code-line-height);
		white-space: pre-wrap;
		word-break: break-word;
		max-height: 180px;
		overflow-y: auto;
		margin: var(--md-sys-space-xs) 0 0;
	}
</style>
