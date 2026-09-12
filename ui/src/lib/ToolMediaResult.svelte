<script>
	import JsonView from '$lib/JsonView.svelte';

	let { data = {} } = $props();
	let media = $derived(data.media ?? {});
	let assetId = $derived(data.asset_id ?? media.asset_id ?? '');
	let filename = $derived(media.filename ?? '');
	let representation = $derived(data.representation ?? media.representation ?? '');
	let availableRepresentations = $derived(
		Array.isArray(media.available_representations) ? media.available_representations : [],
	);
	let recommendedNext = $derived(
		typeof media.recommended_next === 'string' ? media.recommended_next : '',
	);
	let text = $derived(typeof media.content === 'string' ? media.content : '');
	/** @type {Record<string, string>} */
	const operationLabels = {
		record: '录音完成',
		play: '播放完成',
		speak: '朗读完成',
		volume_get: '当前音量',
		volume_set: '音量已设置',
		mute_get: '当前静音状态',
		mute_set: '静音状态已设置',
	};
	let operationLabel = $derived(operationLabels[data.operation] || '媒体操作');
</script>

<div class="media-detail">
		<span class="media-op">{data.operation || '媒体操作'}</span>
		{#if assetId}<span class="media-asset">{assetId}</span>{/if}
		{#if filename}<span class="media-name">{filename}</span>{/if}
</div>

{#if data.operation === 'record'}
	<div class="media-action"><span class="media-badge">{operationLabel}</span><span>{data.duration_ms != null ? `${data.duration_ms} ms` : ''}</span></div>
	{#if assetId}<div class="tool-card-meta">资产：{assetId}</div>{/if}
	{#if representation}<div class="tool-card-meta">表示：{representation}</div>{/if}
	{#if availableRepresentations.length}<div class="tool-card-meta">可用表示：{availableRepresentations.join('、')}</div>{/if}
	{#if recommendedNext}<div class="tool-card-meta">建议下一步：{recommendedNext}</div>{/if}
	{#if data.transcript}<pre class="media-text">{data.transcript}</pre>{/if}
{:else if data.operation === 'play'}
	<div class="media-action"><span class="media-badge">{operationLabel}</span>{#if data.played}<span>已发送到扬声器</span>{/if}</div>
{:else if data.operation === 'speak'}
	<div class="media-action"><span class="media-badge">{operationLabel}</span><span>{data.characters != null ? `${data.characters} 字` : ''}</span></div>
{:else if data.operation === 'volume_get' || data.operation === 'volume_set'}
	<div class="media-action"><span class="media-badge">{operationLabel}</span><span>{Math.round(Number(data.volume ?? 0) * 100)}%</span></div>
{:else if data.operation === 'mute_get' || data.operation === 'mute_set'}
	<div class="media-action"><span class="media-badge">{operationLabel}</span><span>{data.muted ? '已静音' : '未静音'}</span></div>
{:else}
	{#if representation}<div class="tool-card-meta">表示：{representation}</div>{/if}
	{#if availableRepresentations.length}
		<div class="tool-card-meta">可用表示：{availableRepresentations.join('、')}</div>
	{/if}
	{#if recommendedNext}<div class="tool-card-meta">建议下一步：{recommendedNext}</div>{/if}
	{#if data.available === false}
		<p class="tool-card-empty">{data.reason || '此媒体能力当前不可用'}</p>
	{:else if text}
		<pre class="media-text">{text}</pre>
	{:else if data.operation === 'inspect'}
		<div class="tool-card-meta">{data.modality || data.file_kind || '媒体资产'} · 可继续交给 media 工具处理</div>
	{:else}
		<JsonView value={data} defaultDepth={1} />
	{/if}
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
	.media-action { display: flex; align-items: baseline; gap: var(--md-sys-space-xs); min-width: 0; color: var(--md-sys-color-on-surface); }
	.media-badge { flex: none; padding: 1px 6px; border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); font-size: var(--md-sys-typescale-label-small-size); font-weight: 700; }
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
