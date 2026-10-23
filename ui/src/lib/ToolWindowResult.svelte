<script lang="ts">
	import JsonView from '$lib/JsonView.svelte';
	import MaterialBadge from '$lib/MaterialBadge.svelte';
	import ToolResultList from '$lib/ToolResultList.svelte';
	import type {
		ToolWindowControlType,
		ToolWindowFormat,
		ToolWindowOperation,
		ToolWindowWaitCondition,
	} from './toolResultPresentation.ts';

	interface Props {
		data?: {
			media?: { asset_id?: string | null; content?: unknown } | null;
			asset_id?: string | null;
			windows?: Array<{
				window_id: string;
				hwnd?: number | string | null;
				title?: string | null;
				pid?: number | null;
			}>;
			elements?: Array<{
				element_token?: string | null;
				name?: string | null;
				control_type?: ToolWindowControlType | null;
			}>;
			count?: number | null;
			available?: boolean | null;
			note?: string | null;
			operation?: ToolWindowOperation | null;
			title?: string | null;
			pid?: number | null;
			focused?: string | null;
			verified_foreground?: boolean | null;
			foreground_window?: {
				window_id?: string | null;
				hwnd?: number | string | null;
				title?: string | null;
				pid?: number | null;
			} | null;
			closed?: string | null;
			verified_closed?: boolean | null;
			width?: number | null;
			height?: number | null;
			format?: ToolWindowFormat | null;
			success?: boolean | null;
			reason?: string | null;
			matched?: boolean | null;
			timed_out?: boolean | null;
			condition?: ToolWindowWaitCondition | null;
			text?: string | null;
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
				{#each visibleWindows as window (window.window_id)}
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
		<MaterialBadge text="当前窗口" variant="secondary" size="compact" shape="pill" />
		<span class="tool-result-window-primary window-title" title={data.title}
			>{data.title || '(无标题)'}</span
		>
		{#if data.pid}<span class="tool-result-secondary-value window-meta">PID {data.pid}</span
			>{/if}
	</div>
{:else if data.operation === 'focus' || data.operation === 'close'}
	<div class="window-detail">
		<MaterialBadge
			text={data.operation === 'focus' ? '已聚焦' : '已确认关闭'}
			variant="success"
			size="compact"
			shape="pill"
		/>
		{#if data.focused || data.closed}<span class="tool-result-window-primary window-title"
				>{data.focused || data.closed}</span
			>{/if}
		{#if data.pid}<span class="tool-result-secondary-value window-meta">PID {data.pid}</span
			>{/if}
		{#if data.operation === 'focus' && data.verified_foreground === true && data.foreground_window}
			<div class="tool-result-meta">
				前台验证通过：{data.foreground_window.title || data.foreground_window.window_id || '未知窗口'}
				{#if data.foreground_window.pid} · PID {data.foreground_window.pid}{/if}
				{#if data.foreground_window.hwnd != null} · HWND {data.foreground_window.hwnd}{/if}
			</div>
		{/if}
		{#if data.operation === 'close' && data.verified_closed === true}
			<div class="tool-result-meta">窗口句柄已确认消失。</div>
		{/if}
	</div>
{:else if data.operation === 'screenshot'}
	<div class="window-detail">
		<MaterialBadge text="截图已生成" variant="success" size="compact" shape="pill" />
		{#if data.asset_id}<span class="window-asset">{data.asset_id}</span>{/if}
	</div>
	{#if data.width != null && data.height != null}<div class="tool-result-meta">
			{data.width}×{data.height}{data.format ? ` · ${data.format.toUpperCase()}` : ''}
		</div>{/if}
{:else if data.operation === 'ocr'}
	<div class="window-detail">
		<MaterialBadge
			text={data.success === false ? 'OCR 失败' : 'OCR 完成'}
			variant={data.success === false ? 'error' : 'success'}
			size="compact"
			shape="pill"
		/>
		{#if mediaAssetId}<span class="window-asset">{mediaAssetId}</span>{/if}
	</div>
	{#if mediaText}<pre class="tool-result-preview">{mediaText}</pre>{:else if data.reason}<p
			class="tool-result-message"
		>
			{data.reason}
		</p>{/if}
{:else if data.operation === 'wait'}
	<div class="window-detail">
		<MaterialBadge
			text={data.matched ? '已匹配' : data.timed_out ? '等待超时' : '等待结束'}
			variant={data.timed_out ? 'warning' : data.matched ? 'success' : 'secondary'}
			size="compact"
			shape="pill"
		/>
		{#if data.condition}<span class="tool-result-secondary-value window-meta"
				>{data.condition}</span
			>{/if}
	</div>
	{#if data.text}<div class="tool-result-meta">{data.text}</div>{/if}
{:else if Array.isArray(data.elements)}
	<div class="tool-result-label">{data.count ?? data.elements.length} 个界面元素</div>
	{#if data.note}<p class="tool-result-message">{data.note}</p>{/if}
	<ToolResultList items={data.elements}>
		{#snippet children(visibleElements)}
			<div class="tool-result-scroll-area">
				{#each visibleElements as element (element.element_token)}
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
