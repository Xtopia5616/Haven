<script>
	import JsonView from '$lib/JsonView.svelte';

	let { data = {} } = $props();
	/** @type {Record<string, string>} */
	const labels = {
		type: '已输入文本',
		key: '已按下按键',
		click: '已点击',
		move: '已移动指针',
		scroll: '已滚动',
	};
	let label = $derived(labels[data.operation] || '输入操作');
</script>

{#if data.operation === 'type'}
	<div class="input-action"><span class="input-badge">{label}</span><span>{data.chars ?? 0} 个字符</span></div>
	{#if data.typed}<pre class="content-preview">{data.typed}</pre>{/if}
{:else if data.operation === 'key'}
	<div class="input-action"><span class="input-badge">{label}</span><code>{data.pressed || '—'}</code></div>
{:else if data.operation === 'click'}
	<div class="input-action"><span class="input-badge">{label}</span><span>{Array.isArray(data.clicked) ? data.clicked.join(', ') : '—'}</span><span class="input-meta">{data.button || 'left'}</span></div>
{:else if data.operation === 'move'}
	<div class="input-action"><span class="input-badge">{label}</span><span>{Array.isArray(data.moved_to) ? data.moved_to.join(', ') : '—'}</span></div>
{:else if data.operation === 'scroll'}
	<div class="input-action"><span class="input-badge">{label}</span><span>{data.scrolled ?? 0}</span></div>
{:else}
	<div class="tool-card-meta">输入操作</div>
	<JsonView value={data} defaultDepth={1} />
{/if}

<style>
	.input-action { display: flex; align-items: baseline; gap: var(--md-sys-space-xs); color: var(--md-sys-color-on-surface); }
	.input-badge { flex: none; padding: 1px 6px; border-radius: var(--md-sys-shape-full); background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container); font-size: var(--md-sys-typescale-label-small-size); font-weight: 700; }
	.input-meta, .tool-card-meta { color: var(--md-sys-color-on-surface-variant); font-size: var(--md-sys-typescale-label-small-size); }
	.content-preview { max-height: 120px; margin: var(--md-sys-space-xs) 0 0; padding: var(--md-sys-space-xs) var(--md-sys-space-sm); overflow-y: auto; white-space: pre-wrap; word-break: break-word; border-radius: var(--md-sys-shape-small); background: var(--md-sys-color-surface-container-high); color: var(--md-sys-color-on-surface-variant); font-family: var(--md-sys-typescale-mono); font-size: var(--md-sys-typescale-code-size); }
</style>
