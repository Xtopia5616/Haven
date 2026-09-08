<script>
	import ExternalRef from '$lib/ExternalRef.svelte';
	import JsonView from '$lib/JsonView.svelte';

	let { data = {} } = $props();
	/** @type {Record<string, string>} */
	const operationLabels = {
		record: '录音完成',
		play: '播放完成',
		volume_get: '当前音量',
		volume_set: '音量已设置',
		mute_get: '当前静音状态',
		mute_set: '静音状态已设置',
	};
	let operationLabel = $derived(operationLabels[data.operation] || '音频结果');
</script>

{#if data.operation === 'record'}
	<div class="audio-action"><span class="audio-badge">{operationLabel}</span><span>{data.duration_ms != null ? `${data.duration_ms} ms` : ''}</span></div>
	{#if data.transcript}<pre class="content-preview">{data.transcript}</pre>{/if}
{:else if data.operation === 'play'}
	<div class="audio-action"><span class="audio-badge">{operationLabel}</span>{#if data.played}<ExternalRef class="audio-path" target={data.played} />{/if}</div>
{:else if data.operation === 'volume_get' || data.operation === 'volume_set'}
	<div class="audio-action"><span class="audio-badge">{operationLabel}</span><span>{Math.round(Number(data.volume ?? 0) * 100)}%</span></div>
{:else if data.operation === 'mute_get' || data.operation === 'mute_set'}
	<div class="audio-action"><span class="audio-badge">{operationLabel}</span><span>{data.muted ? '已静音' : '未静音'}</span></div>
{:else}
	<JsonView value={data} defaultDepth={1} />
{/if}

<style>
	.audio-action { display: flex; align-items: baseline; gap: var(--md-sys-space-xs); min-width: 0; color: var(--md-sys-color-on-surface); }
	.audio-badge { flex: none; padding: 1px 6px; border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); font-size: var(--md-sys-typescale-label-small-size); font-weight: 700; }
	:global(.audio-path) { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--md-sys-color-primary); }
	.content-preview { max-height: 120px; margin: var(--md-sys-space-xs) 0 0; padding: var(--md-sys-space-xs) var(--md-sys-space-sm); overflow-y: auto; white-space: pre-wrap; word-break: break-word; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-high); color: var(--md-sys-color-on-surface-variant); font-family: var(--md-sys-typescale-mono); font-size: var(--md-sys-typescale-code-size); }
</style>
