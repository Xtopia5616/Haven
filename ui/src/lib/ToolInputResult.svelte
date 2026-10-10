<script lang="ts">
	import JsonView from '$lib/JsonView.svelte';
	import type { ToolInputButton, ToolInputOperation } from './toolResultPresentation.ts';

	interface Props {
		data?: {
			operation?: ToolInputOperation | null;
			chars?: number | null;
			typed?: string | null;
			pressed?: string | null;
			accepted_events?: number | null;
			verified_foreground?: boolean | null;
			foreground_window_id?: string | null;
			foreground_title?: string | null;
			foreground_hwnd?: number | null;
			foreground_pid?: number | null;
			clicked?: [number, number];
			button?: ToolInputButton | null;
			moved_to?: [number, number];
			scrolled?: number | null;
		};
	}

	let { data = {} }: Props = $props();
	const labels: Partial<Record<ToolInputOperation, string>> = {
		type: '已输入文本',
		key: '已按下按键',
		click: '已点击',
		move: '已移动指针',
		scroll: '已滚动',
	};
	let label = $derived((data.operation && labels[data.operation]) || '输入操作');
</script>

{#if data.operation === 'type'}
	<div class="input-action">
		<span class="input-badge">{label}</span><span>{data.chars ?? 0} 个字符</span>
	</div>
	{#if data.typed}<pre class="tool-result-preview">{data.typed}</pre>{/if}
{:else if data.operation === 'key'}
	<div class="input-action">
		<span class="input-badge">按键已注入</span><code>{data.pressed || '—'}</code>
	</div>
	{#if data.verified_foreground === true}
		<div class="tool-result-meta tool-result-meta--input">
			SendInput 接收 {data.accepted_events ?? '未知'} 个事件；前台窗口已验证：{data.foreground_title || data.foreground_window_id || '未知'}
			{#if data.foreground_pid} · PID {data.foreground_pid}{/if}
			{#if data.foreground_hwnd != null} · HWND {data.foreground_hwnd}{/if}
		</div>
	{/if}
{:else if data.operation === 'click'}
	<div class="input-action">
		<span class="input-badge">{label}</span><span
			>{Array.isArray(data.clicked) ? data.clicked.join(', ') : '—'}</span
		><span class="tool-result-meta tool-result-meta--input">{data.button || 'left'}</span>
	</div>
{:else if data.operation === 'move'}
	<div class="input-action">
		<span class="input-badge">{label}</span><span
			>{Array.isArray(data.moved_to) ? data.moved_to.join(', ') : '—'}</span
		>
	</div>
{:else if data.operation === 'scroll'}
	<div class="input-action">
		<span class="input-badge">{label}</span><span>{data.scrolled ?? 0}</span>
	</div>
{:else}
	<div class="tool-result-meta tool-result-meta--input">输入操作</div>
	<JsonView value={data} defaultDepth={1} />
{/if}
{#if data.operation !== 'key' && data.accepted_events != null}
	<div class="tool-result-meta tool-result-meta--input">
		Windows 已接收 {data.accepted_events} 个输入事件；这不证明应用已完成对应操作。
	</div>
{/if}

<style>
	.input-action {
		display: flex;
		align-items: baseline;
		gap: var(--md-sys-space-xs);
		color: var(--md-sys-color-on-surface);
	}
	.input-badge {
		flex: none;
		padding: 1px 6px;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
	}
	.tool-result-preview {
		--tool-result-preview-max-height: 120px;
		--tool-result-preview-line-height: normal;
	}
</style>
