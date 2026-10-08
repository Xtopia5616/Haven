<script lang="ts">
	import JsonView from '$lib/JsonView.svelte';
	import ToolResultList from '$lib/ToolResultList.svelte';
	import type {
		ToolWindowControlType,
		ToolWindowFormat,
		ToolWindowOperation,
		ToolWindowWaitCondition,
	} from './toolResultPresentation.ts';

	interface Props {
		data?: {
			media?: { asset_id?: string; content?: unknown };
			asset_id?: string;
			windows?: Array<{ hwnd?: number | string; title?: string; pid?: number }>;
			elements?: Array<{ name?: string; control_type?: ToolWindowControlType | null }>;
			count?: number;
			available?: boolean;
			note?: string;
			operation?: ToolWindowOperation | null;
			title?: string;
			pid?: number;
			focused?: string;
			closed?: string;
			width?: number;
			height?: number;
			format?: ToolWindowFormat | null;
			success?: boolean;
			reason?: string;
			matched?: boolean;
			timed_out?: boolean;
			condition?: ToolWindowWaitCondition | null;
			text?: string;
		};
	}

	let { data = {} }: Props = $props();
	let media = $derived(data.media ?? {});
	let mediaAssetId = $derived(data.asset_id ?? media.asset_id ?? '');
	let mediaText = $derived(typeof media.content === 'string' ? media.content : '');
</script>

{#if Array.isArray(data.windows)}
	<div class="tool-result-label">{data.count ?? data.windows.length} 个窗口</div>
	{#if data.windows.length > 0}
		<ToolResultList items={data.windows}>
			{#snippet children(visibleWindows)}
				<div class="tool-result-scroll-area">
					{#each visibleWindows as window (window.hwnd ?? window.title)}
						<div class="tool-result-window-row">
							<span
								class="tool-result-window-primary window-title"
								title={window.title}>{window.title || '(无标题)'}</span
							>
							{#if window.pid}<span class="tool-result-secondary-value window-meta"
									>PID {window.pid}</span
								>{/if}
						</div>
					{/each}
				</div>
			{/snippet}
		</ToolResultList>
	{:else}
		<p class="tool-result-message">没有可见窗口</p>
	{/if}
{:else if data.available === false}
	<p class="tool-result-message">{data.note || '此窗口能力当前不可用'}</p>
{:else if data.operation === 'foreground'}
	<div class="window-detail">
		<span class="window-op">当前窗口</span>
		<span class="tool-result-window-primary window-title" title={data.title}
			>{data.title || '(无标题)'}</span
		>
		{#if data.pid}<span class="tool-result-secondary-value window-meta">PID {data.pid}</span
			>{/if}
	</div>
{:else if data.operation === 'focus' || data.operation === 'close'}
	<div class="window-detail">
		<span class="window-op">{data.operation === 'focus' ? '已聚焦' : '已关闭'}</span>
		{#if data.focused || data.closed}<span class="tool-result-window-primary window-title"
				>{data.focused || data.closed}</span
			>{/if}
		{#if data.pid}<span class="tool-result-secondary-value window-meta">PID {data.pid}</span
			>{/if}
	</div>
{:else if data.operation === 'screenshot'}
	<div class="window-detail">
		<span class="window-op">截图已生成</span>
		{#if data.asset_id}<span class="window-asset">{data.asset_id}</span>{/if}
	</div>
	{#if data.width != null && data.height != null}<div class="tool-result-meta">
			{data.width}×{data.height}{data.format ? ` · ${data.format.toUpperCase()}` : ''}
		</div>{/if}
{:else if data.operation === 'ocr'}
	<div class="window-detail">
		<span class="window-op">{data.success === false ? 'OCR 失败' : 'OCR 完成'}</span
		>{#if mediaAssetId}<span class="window-asset">{mediaAssetId}</span>{/if}
	</div>
	{#if mediaText}<pre class="tool-result-preview">{mediaText}</pre>{:else if data.reason}<p
			class="tool-result-message"
		>
			{data.reason}
		</p>{/if}
{:else if data.operation === 'wait'}
	<div class="window-detail">
		<span class="window-op"
			>{data.matched ? '已匹配' : data.timed_out ? '等待超时' : '等待结束'}</span
		>
		{#if data.condition}<span class="tool-result-secondary-value window-meta"
				>{data.condition}</span
			>{/if}
	</div>
	{#if data.text}<div class="tool-result-meta">{data.text}</div>{/if}
{:else if Array.isArray(data.elements)}
	<div class="tool-result-label">{data.count ?? data.elements.length} 个界面元素</div>
	<ToolResultList items={data.elements}>
		{#snippet children(visibleElements)}
			<div class="tool-result-scroll-area">
				{#each visibleElements as element, index (element.name ?? index)}
					<div class="tool-result-window-row">
						<span class="tool-result-window-primary window-title" title={element.name}
							>{element.name || '(未命名元素)'}</span
						>
						{#if element.control_type}<span
								class="tool-result-secondary-value window-meta"
								>{element.control_type}</span
							>{/if}
					</div>
				{/each}
			</div>
		{/snippet}
	</ToolResultList>
{:else if data.operation}
	<div class="tool-result-meta">窗口操作：{data.operation}</div>
	<JsonView value={data} defaultDepth={1} />
{:else}
	<p class="tool-result-message">没有窗口结果</p>
{/if}

<style>
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
	.window-asset {
		min-width: 0;
		flex: 1;
		color: var(--md-sys-color-on-surface-variant);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-code-size);
	}
</style>
