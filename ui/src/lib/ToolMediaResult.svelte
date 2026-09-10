<script>
	import JsonView from '$lib/JsonView.svelte';

	let { data = {} } = $props();
	let media = $derived(data.media ?? {});
	let asset = $derived(media.asset ?? {});
	let representation = $derived(data.representation ?? media.preferred_representation ?? '');
	let text = $derived(typeof data.text === 'string' ? data.text : '');
</script>

<div class="media-detail">
		<span class="media-op">{data.operation || '媒体操作'}</span>
		{#if data.asset_id}<span class="media-asset">{data.asset_id}</span>{/if}
		{#if asset.filename}<span class="media-name">{asset.filename}</span>{/if}
</div>
{#if representation}<div class="tool-card-meta">表示：{representation}</div>{/if}
{#if data.available === false}
	<p class="tool-card-empty">{data.reason || '此媒体能力当前不可用'}</p>
{:else if text}
	<pre class="media-text">{text}</pre>
{:else if data.operation === 'inspect'}
	<div class="tool-card-meta">{data.modality || data.file_kind || '媒体资产'} · 可继续交给 media 工具处理</div>
{:else}
	<JsonView value={data} defaultDepth={1} />
{/if}

<style>
	.media-detail {
		display: flex;
		align-items: baseline;
		gap: var(--md-sys-space-xs);
		min-width: 0;
	}
	.media-op {
		flex: none;
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		padding: 1px 6px;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
	}
	.media-asset,
	.media-name {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-code-size);
		color: var(--md-sys-color-on-surface-variant);
	}
	.media-name {
		color: var(--md-sys-color-on-surface);
	}
	.media-text {
		margin: var(--md-sys-space-xs) 0 0;
		max-height: 16rem;
		overflow: auto;
		white-space: pre-wrap;
		font: inherit;
		line-height: 1.45;
		color: var(--md-sys-color-on-surface);
	}
	.tool-card-empty,
	.tool-card-meta {
		margin: var(--md-sys-space-xs) 0 0;
		color: var(--md-sys-color-on-surface-variant);
	}
</style>
