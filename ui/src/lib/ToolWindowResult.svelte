<script>
	import ExternalRef from '$lib/ExternalRef.svelte';
	import JsonView from '$lib/JsonView.svelte';

	let { data = {} } = $props();
</script>

{#if Array.isArray(data.windows)}
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
{:else if data.available === false}
	<p class="tool-card-empty">{data.note || '此窗口能力当前不可用'}</p>
{:else if data.operation === 'foreground'}
	<div class="window-detail">
		<span class="window-op">当前窗口</span>
		<span class="window-title" title={data.title}>{data.title || '(无标题)'}</span>
		{#if data.pid}<span class="window-pid">PID {data.pid}</span>{/if}
	</div>
{:else if data.operation === 'focus' || data.operation === 'close'}
	<div class="window-detail">
		<span class="window-op">{data.operation === 'focus' ? '已聚焦' : '已关闭'}</span>
		{#if data.focused || data.closed}<span class="window-title">{data.focused || data.closed}</span>{/if}
		{#if data.pid}<span class="window-pid">PID {data.pid}</span>{/if}
	</div>
{:else if data.operation === 'screenshot'}
	<div class="window-detail">
		<span class="window-op">截图已保存</span>
		{#if data.path}<ExternalRef class="window-path" target={data.path} />{/if}
	</div>
	{#if data.width != null && data.height != null}<div class="tool-card-meta">{data.width}×{data.height}{data.format ? ` · ${data.format.toUpperCase()}` : ''}</div>{/if}
{:else if data.operation === 'ocr'}
	<div class="window-detail"><span class="window-op">{data.ocr_error ? 'OCR 失败' : data.ocr_unavailable ? 'OCR 不可用' : data.too_large ? '截图过大' : 'OCR 完成'}</span>{#if data.path}<ExternalRef class="window-path" target={data.path} />{/if}</div>
	{#if data.text}<pre class="content-preview">{data.text}</pre>{:else if data.reason}<p class="tool-card-empty">{data.reason}</p>{/if}
{:else if data.operation === 'wait'}
	<div class="window-detail">
		<span class="window-op">{data.matched ? '已匹配' : data.timed_out ? '等待超时' : '等待结束'}</span>
		{#if data.condition}<span class="window-pid">{data.condition}</span>{/if}
	</div>
	{#if data.text}<div class="tool-card-meta">{data.text}</div>{/if}
{:else if Array.isArray(data.elements)}
	<div class="tool-card-count">{data.count ?? data.elements.length} 个界面元素</div>
	<div class="tool-card-list">
		{#each data.elements as element, index (element.name ?? index)}
			<div class="window-row">
				<span class="window-title" title={element.name}>{element.name || '(未命名元素)'}</span>
				{#if element.control_type}<span class="window-pid">{element.control_type}</span>{/if}
			</div>
		{/each}
	</div>
{:else if data.operation}
	<div class="tool-card-meta">窗口操作：{data.operation}</div>
	<JsonView value={data} defaultDepth={1} />
{:else}
	<p class="tool-card-empty">没有窗口结果</p>
{/if}

<style>
	.tool-card-count {
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		margin-bottom: var(--md-sys-space-xs);
	}
	.tool-card-empty {
		margin: 0;
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.window-row {
		display: flex;
		align-items: baseline;
		gap: var(--md-sys-space-xs);
		padding: 3px var(--md-sys-space-2xs);
		border-radius: 4px;
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.window-detail {
		display: flex;
		align-items: baseline;
		gap: var(--md-sys-space-xs);
	}
	.window-op {
		flex: none;
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-label-small-line-height);
		padding: 1px 6px;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
	}
	:global(.window-path) {
		min-width: 0;
		flex: 1;
		color: var(--md-sys-color-primary);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.window-row:nth-child(odd) {
		background: color-mix(in srgb, var(--md-sys-color-on-surface) 4%, transparent);
	}
	.window-title {
		flex: 1;
		min-width: 0;
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-code-size);
		line-height: var(--md-sys-typescale-code-line-height);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--md-sys-color-on-surface);
	}
	.window-pid {
		flex: none;
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
</style>
