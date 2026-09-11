<script>
	import { untrack } from 'svelte';
	import JsonView from '$lib/JsonView.svelte';
	import { getSelectedTextWithin, openContextMenu } from '$lib/contextMenu.ts';
	import MaterialCollapsible from '$lib/MaterialCollapsible.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialChoiceChip from '$lib/MaterialChoiceChip.svelte';
	import Icon from '$lib/Icon.svelte';
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
		embedded = false,
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
				...(boundAction.exitCode != null ? { exit_code: boundAction.exitCode } : {}),
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

	// Keep live output visible while a tool is running, then collapse the card
	// once its output is complete. Manual clicks after completion persist until
	// the next streaming transition.
	let cardOpen = $state(untrack(() => liveStreaming));
	let lastStreaming = untrack(() => liveStreaming);
	$effect.pre(() => {
		if (liveStreaming === lastStreaming) return;
		if (liveStreaming) cardOpen = true;
		else cardOpen = false;
		lastStreaming = liveStreaming;
	});
	let kind = $derived(parsed?.kind ?? null);
	let data = $derived(/** @type {any} */ (parsed?.data ?? {}));
	let toolIcon = $derived.by(() => {
		if (toolSource === 'mcp') return 'network';
		if (toolSource === 'skill') return 'sparkles';
		if (kind === 'shell') return 'terminal';
		if (kind === 'notify') return 'bell';
		if (kind === 'generic') return 'tools';
		if (kind === 'raw') return 'file';
		if (toolName === 'files' && Array.isArray(data.results)) return 'search';
		if (toolName === 'system') return 'cpu';
		if (toolName === 'process') return 'activity';
		if (toolName === 'window') return 'monitor';
		if (toolName === 'actions') return 'clock';
		if (toolName === 'schedule') return 'bell';
		if (toolName === 'files') return 'file';
		if (toolName === 'http' || toolName === 'web_search') return 'globe';
		if (toolName === 'clipboard') return 'clipboard';
		if (toolName === 'agent') return 'users';
		if (toolName === 'memory') return 'memory';
		if (toolName === 'media') return 'image';
		return 'tools';
	});
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
	/** @param {any} e */
	function handleContextMenu(e) {
		const selected = getSelectedTextWithin(e.currentTarget);
		openContextMenu(e, buildContextMenuItems(selected));
	}

	/** @param {string} selected */
	function buildContextMenuItems(selected) {
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
	}
</script>

{#if type === 'ask'}
	<div
		class="tool-card tool-card--ask"
		class:embedded
		data-state={awaiting ? 'waiting' : resolved ? 'resolved' : 'completed'}
		role="status"
		oncontextmenu={handleContextMenu}
	>
		<div class="ask-header">
			<span class="ask-mark" aria-hidden="true">
				<Icon name="help" size={18} strokeWidth={2.2} />
			</span>
			<div class="ask-heading">
				<span class="ask-eyebrow">需要你的决定</span>
				<strong class="ask-title">Haven 需要你的回答</strong>
			</div>
			<span class="tool-state" data-state={awaiting ? 'waiting' : 'completed'}>
				<span class="tool-state-dot" aria-hidden="true"></span>
				{awaiting ? '等待回答' : '已处理'}
			</span>
		</div>
		{#if content}
			<div class="ask-question-block">
				<span class="ask-question-label">问题</span>
				<p class="ask-question">{content}</p>
			</div>
		{/if}
		{#if awaiting && options && options.length > 0}
			<div class="ask-options-block">
				<div class="ask-section-heading">
					<span>快速选择</span>
					<span class="ask-selection-count"
						>{selectedOptions.length}/{options.length}</span
					>
				</div>
				<div class="ask-options" role="group" aria-label="回答选项">
					{#each options as opt (opt)}
						<MaterialChoiceChip
							label={opt}
							className="ask-option"
							selected={selectedOptions.includes(opt)}
							onSelect={() => toggleAskOption(opt)}
							onKeydown={handleAskKeydown}
						/>
					{/each}
				</div>
			</div>
		{/if}
		{#if awaiting}
			<div class="ask-actions">
				<div class="ask-action-buttons">
					<MaterialButton
						variant="text"
						className="ask-ignore"
						label="忽略"
						onclick={() => onIgnore?.(messageId)}
					/>
					{#if options && options.length > 0}
						<MaterialButton
							variant="filled"
							className="ask-submit"
							label="提交回答"
							disabled={selectedOptions.length === 0}
							onclick={() => onAskSubmit?.(messageId)}
						/>
					{/if}
				</div>
			</div>
		{/if}
		{#if resolved}
			<div class="ask-resolved">
				<span class="ask-resolved-icon" aria-hidden="true">
					<Icon name={resolved.ignored ? 'close' : 'check'} size={14} strokeWidth={2.5} />
				</span>
				<span>
					<strong>{resolved.ignored ? '已忽略' : '回答已记录'}</strong>
					{#if !resolved.ignored}<span class="ask-resolved-answer"
							>已选择：{resolved.answer}</span
						>{/if}
				</span>
			</div>
		{/if}
	</div>
{:else}
	<div
		class="tool-card"
		class:embedded
		data-state={toolState}
		role="status"
		oncontextmenu={handleContextMenu}
	>
		<MaterialCollapsible bind:open={cardOpen} lazy>
			{#snippet header()}
				<span class="tool-card-icon" aria-hidden="true">
					<Icon name={toolIcon} size={12} strokeWidth={2.5} />
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
	/* ChatBubble owns the shared conversation surface. Embedded tool cards
	 * keep their semantic header/details but do not create a second card. */
	.tool-card.embedded {
		background: transparent;
		border: none;
		border-radius: 0;
		padding: 0;
		box-shadow: none;
	}
	.tool-card.embedded[data-state='running'],
	.tool-card.embedded[data-state='failed'],
	.tool-card.embedded[data-state='cancelled'],
	.tool-card.embedded[data-state='timed_out'],
	.tool-card.embedded[data-state='unknown'] {
		background: transparent;
		border: none;
		box-shadow: none;
	}
	.tool-card.embedded :global(.md-collapsible-header) {
		/* The parent ChatBubble owns the surface padding. Removing the nested
		 * inset keeps a collapsed tool call on the same text rail as a chat
		 * bubble, while its icon and secondary edge retain the distinction. */
		min-height: 28px;
		padding: 0;
	}
	.tool-card :global(.md-collapsible-header) {
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm) var(--md-sys-space-xs)
			var(--md-sys-space-2xs);
		border-radius: var(--md-sys-shape-small);
		transition: background-color var(--md-sys-motion-duration-fast)
			var(--md-sys-motion-easing-standard);
	}
	.tool-card :global(.md-collapsible-header:hover) {
		background: color-mix(in srgb, var(--md-sys-color-primary) 7%, transparent);
	}
	.tool-card.embedded :global(.md-collapsible-body) {
		margin-top: var(--md-sys-space-md);
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
	.tool-card--ask {
		position: relative;
		overflow: hidden;
		border-radius: var(--md-sys-shape-medium);
		border-left: 4px solid var(--md-sys-color-primary);
		border-color: color-mix(
			in srgb,
			var(--md-sys-color-primary) 32%,
			var(--md-sys-color-outline-variant)
		);
		background: color-mix(
			in srgb,
			var(--md-sys-color-primary-container) 16%,
			var(--md-sys-color-surface-container-low)
		);
		box-shadow: none;
	}
	.tool-card.embedded.tool-card--ask {
		padding: var(--md-sys-space-md) var(--md-sys-space-lg);
		border: 1px solid
			color-mix(in srgb, var(--md-sys-color-primary) 28%, var(--md-sys-color-outline-variant));
		border-left: 4px solid var(--md-sys-color-primary);
		border-radius: var(--md-sys-shape-medium);
		background: color-mix(in srgb, var(--md-sys-color-primary-container) 20%, transparent);
		box-shadow: none;
	}
	.tool-card--ask[data-state='waiting'] {
		border-color: color-mix(
			in srgb,
			var(--md-sys-color-primary) 52%,
			var(--md-sys-color-outline-variant)
		);
		border-left-color: var(--md-sys-color-primary);
		box-shadow: none;
	}
	.tool-card--ask[data-state='completed'],
	.tool-card--ask[data-state='resolved'] {
		border-left-color: var(--md-sys-color-success);
		border-color: color-mix(
			in srgb,
			var(--md-sys-color-success) 28%,
			var(--md-sys-color-outline-variant)
		);
	}
	.tool-card.embedded.tool-card--ask[data-state='waiting'] {
		background: color-mix(in srgb, var(--md-sys-color-primary-container) 28%, transparent);
	}
	.ask-header {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-md);
		min-width: 0;
	}
	.ask-mark {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 36px;
		height: 36px;
		flex: none;
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-primary);
		color: var(--md-sys-color-on-primary);
	}
	.ask-heading {
		display: flex;
		flex: 1 1 auto;
		flex-direction: column;
		gap: var(--md-sys-space-2xs);
		min-width: 0;
	}
	.ask-eyebrow {
		color: var(--md-sys-color-primary);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		letter-spacing: var(--md-sys-typescale-overline-letter-spacing);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.ask-title {
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-title-medium-size);
		font-weight: 750;
		line-height: var(--md-sys-typescale-title-medium-line-height);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.ask-header .tool-state {
		padding: 5px 9px;
		border: 1px solid color-mix(in srgb, currentColor 20%, transparent);
		border-radius: var(--md-sys-shape-full);
		background: color-mix(in srgb, currentColor 9%, transparent);
	}
	.ask-header .tool-state[data-state='waiting'] {
		color: var(--md-sys-color-primary);
	}
	.ask-header .tool-state[data-state='completed'] {
		color: var(--md-sys-color-success);
	}
	.ask-question-block {
		margin-top: var(--md-sys-space-lg);
		padding: var(--md-sys-space-md) var(--md-sys-space-lg);
		border: 1px solid
			color-mix(in srgb, var(--md-sys-color-primary) 16%, var(--md-sys-color-outline-variant));
		border-radius: var(--md-sys-shape-small);
		background: color-mix(in srgb, var(--md-sys-color-surface) 72%, transparent);
	}
	.ask-question-label,
	.ask-section-heading {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		letter-spacing: var(--md-sys-typescale-overline-letter-spacing);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.ask-question-label {
		color: var(--md-sys-color-primary);
	}
	.ask-options-block {
		margin-top: var(--md-sys-space-lg);
	}
	.ask-section-heading {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-sm);
		margin-bottom: var(--md-sys-space-sm);
	}
	.ask-selection-count {
		padding: 2px 7px;
		border-radius: var(--md-sys-shape-full);
		background: color-mix(in srgb, var(--md-sys-color-primary) 12%, transparent);
		color: var(--md-sys-color-primary);
		font-family: var(--md-sys-typescale-mono);
		letter-spacing: 0;
	}
	.ask-question {
		margin: var(--md-sys-space-xs) 0 0;
		font-size: var(--md-sys-typescale-body-large-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-body-large-line-height);
		color: var(--md-sys-color-on-surface);
	}
	.ask-options {
		display: flex;
		flex-wrap: wrap;
		gap: var(--md-sys-space-sm);
	}
	:global(.md-choice-chip.ask-option) {
		min-width: 0;
		height: 38px;
		padding: 0 var(--md-sys-space-md);
		border: 1px solid var(--md-sys-color-outline);
		background: var(--md-sys-color-surface);
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 650;
		line-height: var(--md-sys-typescale-label-medium-line-height);
		border-radius: var(--md-sys-shape-full);
		box-shadow: none;
		transition:
			background-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}
	:global(.md-choice-chip.ask-option:hover) {
		background: color-mix(
			in srgb,
			var(--md-sys-color-primary-container) 38%,
			var(--md-sys-color-surface)
		);
	}
	:global(.md-choice-chip.ask-option.selected) {
		background: var(--md-sys-color-primary);
		color: var(--md-sys-color-on-primary);
		border-color: var(--md-sys-color-primary);
		box-shadow: none;
	}
	.ask-actions {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-md);
		margin-top: var(--md-sys-space-lg);
		padding-top: var(--md-sys-space-md);
		justify-content: flex-end;
		border-top: 1px solid
			color-mix(in srgb, var(--md-sys-color-primary) 14%, var(--md-sys-color-outline-variant));
	}
	.ask-action-buttons {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		flex: none;
	}
	:global(.md-btn.ask-ignore),
	:global(.md-btn.ask-submit) {
		box-sizing: border-box;
		height: 36px;
		min-height: 36px;
		min-width: 0;
		border-radius: var(--md-sys-shape-small);
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	:global(.md-btn.ask-ignore) {
		--_btn-fg: var(--md-sys-color-on-surface-variant);
		--_btn-state: var(--md-sys-color-on-surface-variant);
		padding: 0 var(--md-sys-space-sm);
	}
	:global(.md-btn.ask-submit) {
		padding-inline: var(--md-sys-space-lg);
		font-weight: 700;
	}
	.ask-resolved {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		margin-top: var(--md-sys-space-lg);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border: 1px solid
			color-mix(in srgb, var(--md-sys-color-success) 24%, var(--md-sys-color-outline-variant));
		border-radius: var(--md-sys-shape-small);
		background: color-mix(in srgb, var(--md-sys-color-success-container) 52%, transparent);
		color: var(--md-sys-color-on-surface);
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
	}
	.ask-resolved-icon {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 24px;
		height: 24px;
		flex: none;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-success);
		color: var(--md-sys-color-on-success-container);
	}
	.ask-resolved > span:last-child {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-2xs);
	}
	.ask-resolved-answer {
		color: var(--md-sys-color-on-surface-variant);
	}
	.tool-card-icon {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 28px;
		height: 28px;
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-surface-container-highest);
		color: var(--md-sys-color-primary);
		font-size: 12px;
		font-weight: 700;
		flex: none;
	}
	.tool-card-label {
		font-size: var(--md-sys-typescale-label-large-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-label-large-line-height);
		color: var(--md-sys-color-on-surface);
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
		:global(.md-choice-chip.ask-option) {
			transition: none;
		}
	}
	@media (max-width: 520px) {
		.tool-card {
			padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		}
		.ask-action-buttons {
			width: 100%;
			justify-content: flex-end;
		}
		.ask-question-block {
			padding-inline: var(--md-sys-space-md);
		}
	}
</style>
