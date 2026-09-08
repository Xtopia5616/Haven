<script module>
	export { canRenderToolResult, parseToolResult } from './toolResultParsing.ts';
</script>

<script>
	import { untrack } from 'svelte';
	import JsonView from '$lib/JsonView.svelte';
	import ContextMenu from '$lib/ContextMenu.svelte';
	import MaterialCollapsible from '$lib/MaterialCollapsible.svelte';
	import { getToolResultRenderer } from '$lib/toolResultRenderers.ts';
	import { parseToolResult } from '$lib/toolResultParsing.ts';
	import { copyText } from '$lib/clipboard.ts';
	import { actionStore, toolOutputPreviewStore } from '$lib/stores.ts';
	import { formatTokenCount } from '$lib/sessionUsage.ts';
	import { estimateToolDataTokens } from '$lib/sessionUsagePresentation.ts';
	import {
		classifyToolSource,
		parseToolArgs,
		toolDisplayName,
		toolSourceLabel,
	} from '$lib/toolIdentity.ts';
	import { TOOL_INTENT_FALLBACK } from '$lib/toolIntent.ts';

	let {
		type = 'tool',
		toolName = '',
		unrecoverable = false,
		outcome = null,
		content = '',
		options = [],
		awaiting = false,
		messageId = '',
		onAskSelectionChange = null,
		onIgnore = null,
		onAskSubmit = null,
		resolved = null,
		streaming = false,
		actionId = null,
		toolArgs = null,
		showFallbackIntent = false,
	} = $props();

	let toolSource = $derived(classifyToolSource(toolName));
	let sourceBadge = $derived(toolSourceLabel(toolSource));
	let displayName = $derived(toolDisplayName(toolName));
	let hasToolArgs = $derived(toolArgs != null && toolArgs !== '');
	const outcomeLabels = /** @type {Record<string, string>} */ ({
		failed: '执行失败',
		cancelled: '已取消',
		timed_out: '执行超时',
		unknown: '结果未知，可能已执行',
	});
	let outcomeLabel = $derived(outcomeLabels[outcome || ''] || '');

	// Local multi-select for ask option chips. Click toggles; Enter in the
	// chat input submits (page composes selected options + any typed text).
	/** @type {string[]} */
	let selectedOptions = $state([]);

	/** @param {string} opt */
	function toggleAskOption(opt) {
		if (!awaiting) return;
		selectedOptions = selectedOptions.includes(opt)
			? selectedOptions.filter((x) => x !== opt)
			: [...selectedOptions, opt];
		onAskSelectionChange?.(messageId, selectedOptions);
	}

	// Enter on a focused option chip submits the composed answers (native
	// button Enter would re-trigger the click and toggle the selection off,
	// which swallowed the submit). Space still toggles the chip.
	/** @param {KeyboardEvent} e */
	function handleAskKeydown(e) {
		if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
			e.preventDefault();
			onAskSubmit?.(messageId);
		}
	}

	// Drop stale selections when the card leaves the awaiting state (answered,
	// ignored, or session resumed) so a later ask never inherits them.
	$effect(() => {
		if (!awaiting && selectedOptions.length > 0) {
			selectedOptions = [];
		}
	});

	const TERMINAL_ACTION = new Set(['completed', 'failed', 'cancelled']);

	// Foreground live tail (side-channel; not written into the message list).
	let livePreview = $derived(
		messageId
			? /** @type {string|undefined} */ ($toolOutputPreviewStore[messageId])
			: undefined,
	);
	// Background actions keep streaming via actionStore after the tool call
	// itself returns `{ background: true, action_id }`. Parent clears actionId
	// once finished output is persisted onto the message.
	let boundAction = $derived(
		actionId ? /** @type {any} */ ($actionStore[actionId] || null) : null,
	);
	let actionRunning = $derived(!!boundAction && boundAction.status === 'running');
	let liveStreaming = $derived(streaming || actionRunning || !!livePreview);
	let displayContent = $derived.by(() => {
		if (actionRunning) {
			const out =
				typeof boundAction.output === 'string' ? boundAction.output : livePreview || '';
			return JSON.stringify({
				output: out,
				background: true,
				action_id: actionId,
				status: 'running',
			});
		}
		if (boundAction && TERMINAL_ACTION.has(boundAction.status)) {
			const rawOut =
				typeof boundAction.output === 'string'
					? boundAction.output
					: typeof boundAction.error === 'string'
						? boundAction.error
						: '';
			if (typeof rawOut === 'string' && rawOut.trim().startsWith('{')) {
				return rawOut;
			}
			return JSON.stringify({
				output: rawOut,
				background: true,
				action_id: actionId,
				status: boundAction.status,
				...(boundAction.exit_code != null ? { exit_code: boundAction.exit_code } : {}),
				...(boundAction.error && !boundAction.output ? { error: boundAction.error } : {}),
			});
		}
		if (livePreview != null && livePreview !== '') {
			return JSON.stringify({ output: livePreview });
		}
		return content;
	});
	let toolDataUsage = $derived(
		type === 'tool' ? estimateToolDataTokens(toolName, toolArgs, displayContent) : null,
	);

	let parsed = $derived(type === 'tool' ? parseToolResult(toolName, displayContent) : null);

	// Tool details are useful after completion as well as during execution, so
	// cards start open and stay open. The user can still collapse a card
	// manually; live output is filled into the same body as events arrive.
	let cardOpen = $state(true);
	let lastStreaming = untrack(() => liveStreaming);
	$effect.pre(() => {
		if (liveStreaming === lastStreaming) return;
		if (liveStreaming) cardOpen = true;
		lastStreaming = liveStreaming;
	});
	let kind = $derived(parsed?.kind ?? null);
	let data = $derived(/** @type {any} */ (parsed?.data ?? {}));
	let BodyRenderer = $derived(getToolResultRenderer(kind, toolName, data));
	const toolStateLabels = /** @type {Record<string, string>} */ ({
		running: '执行中',
		completed: '完成',
		failed: '失败',
		cancelled: '已取消',
		timed_out: '超时',
		unknown: '未知',
	});
	let toolState = $derived(outcome || (liveStreaming ? 'running' : 'completed'));
	let toolStateLabel = $derived(toolStateLabels[toolState] || toolState);
	// The `raw` kind carries data: null for plain text and the parsed JSON
	// value for arrays/primitives; `data` above would collapse the null to {},
	// so resolve the body text here against the original `parsed` payload.
	let rawText = $derived(
		kind === 'raw'
			? parsed?.data == null
				? displayContent
				: JSON.stringify(parsed?.data, null, 2)
			: '',
	);
	let shellText = $derived(
		kind === 'shell'
			? typeof data.output === 'string'
				? data.output
				: Object.keys(data).length > 0
					? JSON.stringify(data, null, 2)
					: displayContent
			: '',
	);
	function notifyPartsOf() {
		if (kind !== 'notify') return { title: '', body: '' };
		const rest = content.slice('Notification sent:'.length).trim();
		const idx = rest.indexOf(': ');
		return idx > 0
			? { title: rest.slice(0, idx).trim(), body: rest.slice(idx + 2).trim() }
			: { title: rest, body: '' };
	}
	let notifyParts = $derived(notifyPartsOf());

	// Right-click: copy the visible observation (or the current selection),
	// matching the chat-bubble menu. Live shell output uses displayContent
	// rather than the still-empty message `content`.
	let copyableOutput = $derived.by(() => {
		if (type === 'ask') return typeof content === 'string' ? content : '';
		if (kind === 'shell') return shellText || '';
		if (kind === 'raw') return rawText || '';
		if (kind === 'notify') {
			return (
				[notifyParts.title, notifyParts.body].filter(Boolean).join('\n') || content || ''
			);
		}
		if (parsed?.data != null) {
			try {
				return JSON.stringify(parsed.data, null, 2);
			} catch {
				return displayContent || content || '';
			}
		}
		return displayContent || content || '';
	});
	let ctxMenu = $state({ open: false, x: 0, y: 0, selected: '' });

	/** @param {any} e */
	function handleContextMenu(e) {
		e.preventDefault();
		e.stopPropagation();
		let selected = '';
		const selection = window.getSelection();
		if (selection && !selection.isCollapsed && selection.toString().trim()) {
			const el = e.currentTarget;
			if (el && el.contains(selection.anchorNode) && el.contains(selection.focusNode)) {
				selected = selection.toString().trim();
			}
		}
		ctxMenu = { open: true, x: e.clientX, y: e.clientY, selected };
	}

	function closeCtxMenu() {
		ctxMenu = { open: false, x: 0, y: 0, selected: '' };
	}

	let ctxMenuItems = $derived.by(() => {
		const selected = ctxMenu.selected;
		const copyAllLabel = type === 'ask' ? '复制问题' : '复制输出';
		/** @type {any[]} */
		const items = [];
		if (selected) {
			items.push({
				id: 'copySel',
				label: '复制选中',
				icon: 'copy',
				action: () => copyText(selected, '选中'),
			});
		}
		items.push({
			id: 'copy',
			label: selected ? '复制全部' : copyAllLabel,
			icon: 'copy',
			action: () => copyText(copyableOutput, type === 'ask' ? '问题' : '输出'),
		});
		if (type !== 'ask' && hasToolArgs) {
			items.push({
				id: 'copyArgs',
				label: '复制参数',
				icon: 'copy',
				action: () => {
					const parsed = parseToolArgs(toolArgs);
					if (parsed == null) return;
					const text =
						typeof toolArgs === 'string'
							? toolArgs
							: (() => {
									try {
										return JSON.stringify(parsed, null, 2);
									} catch {
										return String(parsed);
									}
								})();
					copyText(text, '参数');
				},
			});
		}
		return items;
	});
</script>

{#if type === 'ask'}
	<div
		class="tool-card tool-card--ask"
		data-state={awaiting ? 'waiting' : resolved ? 'resolved' : 'completed'}
		role="status"
		oncontextmenu={handleContextMenu}
	>
		<div class="tool-card-header">
			<span class="tool-card-icon" aria-hidden="true">&#63;</span>
			<span class="tool-card-label">Haven 需要你的回答</span>
			<span class="tool-state" data-state={awaiting ? 'waiting' : 'completed'}>
				<span class="tool-state-dot" aria-hidden="true"></span>
				{awaiting ? '等待回答' : '已处理'}
			</span>
		</div>
		{#if content}
			<p class="ask-question">{content}</p>
		{/if}
		{#if awaiting && options && options.length > 0}
			<div class="ask-options">
				{#each options as opt (opt)}
					<button
						class="ask-option"
						class:selected={selectedOptions.includes(opt)}
						aria-pressed={selectedOptions.includes(opt)}
						onclick={() => toggleAskOption(opt)}
						onkeydown={handleAskKeydown}
						type="button">{opt}</button
					>
				{/each}
			</div>
		{/if}
		{#if awaiting}
			<div class="ask-actions">
				<button class="ask-ignore" onclick={() => onIgnore?.(messageId)} type="button"
					>忽略</button
				>
				<span class="ask-waiting">
					<span class="ask-waiting-dot"></span>
					{#if options && options.length > 0}
						选择后回车提交
					{:else}
						等待你的回答...
					{/if}
				</span>
				{#if options && options.length > 0}
					<button
						class="ask-submit"
						disabled={selectedOptions.length === 0}
						onclick={() => onAskSubmit?.(messageId)}
						type="button">提交回答</button
					>
				{/if}
			</div>
		{/if}
		{#if resolved}
			<div class="ask-resolved">
				{#if resolved.ignored}
					已忽略
				{:else}
					已选择：{resolved.answer}
				{/if}
			</div>
		{/if}
	</div>
{:else}
	<div class="tool-card" data-state={toolState} role="status" oncontextmenu={handleContextMenu}>
		<MaterialCollapsible bind:open={cardOpen} lazy>
			{#snippet header()}
				<span class="tool-card-icon" aria-hidden="true">
					{#if kind === 'shell'}
						<svg
							width="12"
							height="12"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2.5"
							stroke-linecap="round"
							stroke-linejoin="round"
							><polyline points="4 17 10 11 4 5" /><line
								x1="12"
								y1="19"
								x2="20"
								y2="19"
							/></svg
						>
					{:else if kind === 'notify'}
						<svg
							width="12"
							height="12"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2.5"
							stroke-linecap="round"
							stroke-linejoin="round"
							><path d="M18 8a6 6 0 0 0-12 0c0 7-3 9-3 9h18s-3-2-3-9" /><path
								d="M13.73 21a2 2 0 0 1-3.46 0"
							/></svg
						>
					{:else if kind === 'generic'}
						<svg
							width="12"
							height="12"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2.5"
							stroke-linecap="round"
							stroke-linejoin="round"
							><path
								d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z"
							/></svg
						>
					{:else if kind === 'raw'}
						<svg
							width="12"
							height="12"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2.5"
							stroke-linecap="round"
							stroke-linejoin="round"><path d="M4 6h16M4 12h16M4 18h10" /></svg
						>
					{:else if toolName === 'files'}
						<svg
							width="12"
							height="12"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2.5"
							stroke-linecap="round"
							><circle cx="11" cy="11" r="7" /><line
								x1="21"
								y1="21"
								x2="16.65"
								y2="16.65"
							/></svg
						>
					{:else if toolName === 'system'}
						<svg
							width="12"
							height="12"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2.5"
							stroke-linecap="round"
							stroke-linejoin="round"
							><rect x="4" y="4" width="16" height="16" rx="2" /><rect
								x="9"
								y="9"
								width="6"
								height="6"
							/><path
								d="M9 1v3M15 1v3M9 20v3M15 20v3M20 9h3M20 15h3M1 9h3M1 15h3"
							/></svg
						>
					{:else if toolName === 'process'}
						<svg
							width="12"
							height="12"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2.5"
							stroke-linecap="round"
							stroke-linejoin="round"
							><polyline points="22 12 18 12 15 21 9 3 6 12 2 12" /></svg
						>
					{:else if toolName === 'window'}
						<svg
							width="12"
							height="12"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2.5"
							stroke-linecap="round"
							stroke-linejoin="round"
							><rect x="2" y="3" width="20" height="14" rx="2" /><path
								d="M8 21h8M12 17v4"
							/></svg
						>
					{:else if toolName === 'actions'}
						<svg
							width="12"
							height="12"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2.5"
							stroke-linecap="round"
							stroke-linejoin="round"
							><circle cx="12" cy="12" r="9" /><polyline
								points="12 7 12 12 15.5 13.5"
							/></svg
						>
					{:else if toolName === 'schedule'}
						<svg
							width="12"
							height="12"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2.5"
							stroke-linecap="round"
							stroke-linejoin="round"
							><path d="M18 8a6 6 0 0 0-12 0c0 7-3 9-3 9h18s-3-2-3-9" /><path
								d="M13.73 21a2 2 0 0 1-3.46 0"
							/></svg
						>
					{:else if toolName === 'files' && !Array.isArray(data.results)}
						<svg
							width="12"
							height="12"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2.5"
							stroke-linecap="round"
							stroke-linejoin="round"
							><path
								d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"
							/><polyline points="14 2 14 8 20 8" /></svg
						>
					{:else if toolName === 'http'}
						<svg
							width="12"
							height="12"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2.5"
							stroke-linecap="round"
							stroke-linejoin="round"
							><circle cx="12" cy="12" r="10" /><path
								d="M2 12h20M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z"
							/></svg
						>
					{:else if toolName === 'clipboard'}
						<svg
							width="12"
							height="12"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2.5"
							stroke-linecap="round"
							stroke-linejoin="round"
							><path
								d="M16 4h2a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2"
							/><rect x="8" y="2" width="8" height="4" rx="1" /></svg
						>
					{:else if toolName === 'web_search'}
						<svg
							width="12"
							height="12"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2.5"
							stroke-linecap="round"
							stroke-linejoin="round"
							><circle cx="12" cy="12" r="10" /><path
								d="M2 12h20M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z"
							/></svg
						>
					{:else if toolName === 'agent'}
						<svg
							width="12"
							height="12"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2.5"
							stroke-linecap="round"
							stroke-linejoin="round"
							><path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2" /><circle
								cx="9"
								cy="7"
								r="4"
							/><path d="M23 21v-2a4 4 0 0 0-3-3.87" /><path
								d="M16 3.13a4 4 0 0 1 0 7.75"
							/></svg
						>
					{/if}
				</span>
				<span class="tool-source" data-source={toolSource}>{sourceBadge}</span>
				{#if showFallbackIntent}<span class="tool-intent">{TOOL_INTENT_FALLBACK}</span>{/if}
				<span class="tool-card-label" title={toolName}>{displayName}</span>
				{#if unrecoverable}
					<span
						class="tool-unrecoverable"
						title="该历史操作已移除，不能从当前工具目录恢复">历史操作不可恢复</span
					>
				{/if}
				{#if outcomeLabel}
					<span
						class="tool-outcome"
						data-outcome={outcome}
						title={outcome === 'unknown'
							? '该操作可能已经产生副作用，禁止自动重试'
							: undefined}>{outcomeLabel}</span
					>
				{/if}
				{#if toolDataUsage}
					<span
						class="usage-chip"
						title={[
							`调用参数 ${formatTokenCount(toolDataUsage.args)}`,
							`返回结果 ${formatTokenCount(toolDataUsage.result)}`,
							`合计 ${formatTokenCount(toolDataUsage.total)} tokens`,
						].join('\n')}
					>
						{formatTokenCount(toolDataUsage.total)} tokens
					</span>
				{/if}
				<span class="tool-expand-hint">{cardOpen ? '收起详情' : '查看详情'}</span>
			{/snippet}

			<div class="tool-details">
				<section class="tool-detail" data-detail="status">
					<div class="tool-detail-label">执行状态</div>
					<span class="tool-state" data-state={toolState}>
						<span class="tool-state-dot" aria-hidden="true"></span>
						{toolStateLabel}
					</span>
				</section>

				<section class="tool-detail" data-detail="args">
					<div class="tool-detail-label">调用参数</div>
					{#if hasToolArgs}
						{@const argsValue = parseToolArgs(toolArgs)}
						{#if argsValue != null}
							<div class="tool-args">
								<JsonView value={argsValue} defaultDepth={0} />
							</div>
						{:else}
							<p class="tool-card-empty">（无参数）</p>
						{/if}
					{:else}
						<p class="tool-card-empty">（无参数）</p>
					{/if}
				</section>

				<section class="tool-detail tool-detail--output" data-detail="output">
					<div class="tool-detail-label">输出结果</div>
					{#if BodyRenderer}
						<BodyRenderer
							kind={kind ?? undefined}
							{data}
							{shellText}
							{liveStreaming}
							{rawText}
							parts={notifyParts}
						/>
					{:else if liveStreaming}
						<p class="tool-card-empty">等待输出…</p>
					{:else}
						<p class="tool-card-empty">（无输出）</p>
					{/if}
					{#if data.hint}
						<div class="tool-card-hint">{data.hint}</div>
					{/if}
				</section>
			</div>
		</MaterialCollapsible>
	</div>
{/if}

<ContextMenu
	open={ctxMenu.open}
	x={ctxMenu.x}
	y={ctxMenu.y}
	items={ctxMenuItems}
	onClose={closeCtxMenu}
/>

<style>
	.tool-card {
		background: color-mix(
			in srgb,
			var(--md-sys-color-secondary-container) 30%,
			var(--md-sys-color-surface-container-low)
		);
		border: 1px solid
			color-mix(
				in srgb,
				var(--md-sys-color-secondary) 22%,
				var(--md-sys-color-outline-variant)
			);
		border-left: 3px solid color-mix(in srgb, var(--md-sys-color-secondary) 65%, transparent);
		border-radius: var(--md-sys-shape-large);
		padding: var(--md-sys-space-md) var(--md-sys-space-lg);
		width: 100%;
		max-width: 100%;
		box-sizing: border-box;
		margin-top: 0;
		box-shadow: var(--md-sys-elevation-1);
		transition:
			border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			background-color var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard),
			box-shadow var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.tool-card[data-state='running'] {
		border-color: color-mix(
			in srgb,
			var(--md-sys-color-tertiary) 60%,
			var(--md-sys-color-outline-variant)
		);
		border-left-color: var(--md-sys-color-tertiary);
		background: color-mix(
			in srgb,
			var(--md-sys-color-tertiary-container) 28%,
			var(--md-sys-color-surface)
		);
		box-shadow: var(--md-sys-elevation-2);
	}
	.tool-card[data-state='failed'],
	.tool-card[data-state='cancelled'],
	.tool-card[data-state='timed_out'],
	.tool-card[data-state='unknown'] {
		border-color: color-mix(
			in srgb,
			var(--md-sys-color-error) 45%,
			var(--md-sys-color-outline-variant)
		);
		border-left-color: var(--md-sys-color-error);
		background: color-mix(
			in srgb,
			var(--md-sys-color-error-container) 22%,
			var(--md-sys-color-surface)
		);
	}
	.tool-card--ask[data-state='waiting'] {
		border-color: color-mix(
			in srgb,
			var(--md-sys-color-warning) 58%,
			var(--md-sys-color-outline-variant)
		);
		background: color-mix(
			in srgb,
			var(--md-sys-color-warning-container) 28%,
			var(--md-sys-color-surface)
		);
		border-left-color: var(--md-sys-color-warning);
	}
	.tool-card-header {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		margin-bottom: var(--md-sys-space-xs);
		min-width: 0;
	}
	.tool-card-icon {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 28px;
		height: 28px;
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
		font-size: 12px;
		font-weight: 700;
		flex: none;
	}
	.tool-card-label {
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-secondary-container);
		min-width: 0;
		flex: 1 1 10rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	:global(.tool-card .md-collapsible-header-content) {
		flex-wrap: wrap;
		row-gap: var(--md-sys-space-xs);
	}
	.tool-state {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		flex: none;
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		white-space: nowrap;
	}
	.tool-state[data-state='running'],
	.tool-state[data-state='waiting'] {
		color: var(--md-sys-color-tertiary);
	}
	.tool-state[data-state='failed'],
	.tool-state[data-state='cancelled'],
	.tool-state[data-state='timed_out'],
	.tool-state[data-state='unknown'] {
		color: var(--md-sys-color-error);
	}
	.tool-state-dot {
		width: var(--md-sys-space-sm);
		height: var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-full);
		background: currentColor;
		flex: none;
	}
	.tool-state[data-state='running'] .tool-state-dot,
	.tool-state[data-state='waiting'] .tool-state-dot {
		animation: tool-state-pulse 1.2s ease-in-out infinite;
	}
	@keyframes tool-state-pulse {
		0%,
		100% {
			opacity: 0.45;
			transform: scale(0.8);
		}
		50% {
			opacity: 1;
			transform: scale(1);
		}
	}
	.tool-expand-hint {
		flex: none;
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-primary);
		white-space: nowrap;
	}
	.tool-intent {
		flex: none;
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.tool-unrecoverable {
		flex: none;
		color: var(--md-sys-color-error);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.tool-outcome {
		flex: none;
		color: var(--md-sys-color-error);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.tool-source {
		flex: none;
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		letter-spacing: var(--md-sys-typescale-overline-letter-spacing);
		line-height: var(--md-sys-typescale-label-small-line-height);
		padding: 2px 6px;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-surface-container-highest);
		color: var(--md-sys-color-on-surface-variant);
	}
	.tool-source[data-source='mcp'] {
		background: color-mix(in srgb, var(--md-sys-color-tertiary-container) 80%, transparent);
		color: var(--md-sys-color-on-tertiary-container);
	}
	.tool-source[data-source='skill'] {
		background: color-mix(in srgb, var(--md-sys-color-primary-container) 80%, transparent);
		color: var(--md-sys-color-on-primary-container);
	}
	.tool-details {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-sm);
	}
	.tool-detail {
		min-width: 0;
	}
	.tool-detail + .tool-detail {
		border-top: 1px solid var(--md-sys-color-outline-variant);
		padding-top: var(--md-sys-space-sm);
	}
	.tool-detail-label {
		margin-bottom: var(--md-sys-space-xs);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.tool-detail[data-detail='status'] {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-sm);
	}
	.tool-detail[data-detail='status'] .tool-detail-label {
		margin-bottom: 0;
	}
	.tool-args {
		min-width: 0;
	}
	.ask-question {
		margin: 0 0 var(--md-sys-space-sm);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
		color: var(--md-sys-color-on-surface);
	}
	.ask-options {
		display: flex;
		flex-wrap: wrap;
		gap: var(--md-sys-space-xs);
		margin-bottom: var(--md-sys-space-sm);
	}
	.ask-option {
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-full);
		padding: var(--md-sys-space-xs) var(--md-sys-space-md);
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-medium-line-height);
		cursor: pointer;
		transition:
			filter 0.15s ease,
			background 0.15s ease,
			border-color 0.15s ease,
			color 0.15s ease;
	}
	.ask-option:hover {
		filter: brightness(0.95);
	}
	.ask-option.selected {
		background: var(--md-sys-color-primary);
		color: var(--md-sys-color-on-primary);
		border-color: var(--md-sys-color-primary);
		filter: none;
	}
	.ask-actions {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		margin-bottom: var(--md-sys-space-sm);
	}
	.ask-ignore {
		background: transparent;
		color: var(--md-sys-color-on-surface-variant);
		border: 1px solid var(--md-sys-color-outline);
		border-radius: var(--md-sys-shape-full);
		padding: var(--md-sys-space-2xs) var(--md-sys-space-sm);
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		cursor: pointer;
		transition: filter 0.15s ease;
	}
	.ask-submit {
		margin-left: auto;
		border: 1px solid var(--md-sys-color-primary);
		border-radius: var(--md-sys-shape-full);
		padding: var(--md-sys-space-xs) var(--md-sys-space-md);
		background: var(--md-sys-color-primary);
		color: var(--md-sys-color-on-primary);
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-label-medium-line-height);
		cursor: pointer;
		transition:
			background-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			opacity var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}
	.ask-submit:hover:not(:disabled) {
		background: color-mix(
			in srgb,
			var(--md-sys-color-primary) 88%,
			var(--md-sys-color-on-primary)
		);
	}
	.ask-submit:focus-visible,
	.ask-ignore:focus-visible {
		outline: 2px solid var(--md-sys-color-primary);
		outline-offset: 2px;
	}
	.ask-submit:disabled {
		cursor: not-allowed;
		opacity: 0.45;
	}
	.ask-ignore:hover {
		filter: brightness(0.9);
	}
	.ask-resolved {
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.ask-waiting {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.ask-waiting-dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--md-sys-color-secondary);
		animation: ask-pulse 1.2s ease-in-out infinite;
	}
	@keyframes ask-pulse {
		0%,
		100% {
			opacity: 1;
			transform: scale(1);
		}
		50% {
			opacity: 0.35;
			transform: scale(0.8);
		}
	}
	.tool-card-empty {
		margin: 0;
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	.usage-chip {
		display: inline-block;
		flex: none;
		padding: 1px 8px;
		border-radius: var(--md-sys-shape-full);
		background: color-mix(in srgb, var(--md-sys-color-tertiary) 14%, transparent);
		color: var(--md-sys-color-on-surface-variant);
		border: 1px solid color-mix(in srgb, var(--md-sys-color-tertiary) 30%, transparent);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 600;
		font-family: var(--md-sys-typescale-mono);
		line-height: var(--md-sys-typescale-label-small-line-height);
		white-space: nowrap;
		cursor: default;
	}
	.tool-card-hint {
		margin-top: var(--md-sys-space-xs);
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		border-top: 1px dashed var(--md-sys-color-outline-variant);
		padding-top: var(--md-sys-space-xs);
	}
	@media (prefers-reduced-motion: reduce) {
		.tool-state[data-state='running'] .tool-state-dot,
		.tool-state[data-state='waiting'] .tool-state-dot {
			animation: none;
		}
	}
	@media (max-width: 520px) {
		.tool-card {
			padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		}
		.tool-expand-hint {
			width: 100%;
			margin-left: var(--md-sys-space-lg);
		}
		.ask-actions {
			align-items: flex-start;
			flex-wrap: wrap;
		}
		.ask-waiting {
			flex: 1 1 8rem;
		}
		.ask-submit {
			margin-left: 0;
		}
	}
</style>
