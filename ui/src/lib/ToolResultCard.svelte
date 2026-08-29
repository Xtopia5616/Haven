<script module>
	// Structured renderers for tool observations whose output is JSON, plus the
	// `ask` question card. Kept in one place so live chat and history resume
	// share the same cards with a unified look. JSON without a dedicated
	// renderer falls through to the generic JsonView tree below.

	/** @param {unknown} v */
	function isObj(v) {
		return typeof v === 'object' && v !== null && !Array.isArray(v);
	}

	/**
	 * Whether this tool observation can be rendered as a card. Every non-empty
	 * observation is renderable — structured JSON gets its dedicated renderer
	 * and anything else falls back to the `raw` card — so this is only false
	 * for empty content.
	 * @param {string} toolName
	 * @param {string} content
	 * @returns {boolean}
	 */
	export function canRenderToolResult(toolName, content) {
		return parseToolResult(toolName, content) !== null;
	}

	/**
	 * Parse + classify a tool observation into a renderable payload.
	 * kind is one of:
	 * - 'custom': a dedicated renderer below matches the JSON shape
	 * - 'generic': JSON object with no dedicated renderer (JSON tree)
	 * - 'shell': terminal output (plain text or {"output": ...})
	 * - 'notify': readable "Notification sent: ..." text
	 * - 'raw': anything else — plain text, JSON arrays / primitives, or
	 *   invalid JSON — pretty-printed in a generic card
	 * Returns null only for empty content on non-shell tools (the card is then
	 * omitted). Empty shell content still yields a shell card so streaming /
	 * background placeholders can render.
	 * @param {string} toolName
	 * @param {string} content
	 * @returns {{ kind: string, data: object | null } | null}
	 */
	export function parseToolResult(toolName, content) {
		// Empty content is still a shell card while streaming / waiting for
		// the first live-output chunk (or a background action bind).
		if (!content) {
			return toolName === 'shell' ? { kind: 'shell', data: null } : null;
		}
		if (toolName === 'shell') {
			let data = null;
			try {
				const j = JSON.parse(content);
				if (isObj(j)) data = j;
			} catch {
				// Plain text output — still renderable in the terminal card.
			}
			return { kind: 'shell', data };
		}
		if (toolName === 'notify' && content.startsWith('Notification sent:')) {
			return { kind: 'notify', data: null };
		}
		let data;
		try {
			data = JSON.parse(content);
		} catch {
			// Not JSON — plain text, rendered in the raw card.
			return { kind: 'raw', data: null };
		}
		if (!isObj(data)) {
			// JSON arrays / primitives — pretty-printed in the raw card.
			return { kind: 'raw', data };
		}
		const custom = customShape(toolName, data);
		return custom ? { kind: 'custom', data } : { kind: 'generic', data };
	}

	/**
	 * Match a JSON observation against a dedicated renderer shape.
	 * @param {string} toolName
	 * @param {any} data
	 * @returns {object | null}
	 */
	function customShape(toolName, data) {
		switch (toolName) {
			case 'file_search':
			case 'files':
				if (Array.isArray(data.results)) return data;
				if (
					data.written ||
					data.edited ||
					data.copied ||
					data.moved ||
					data.deleted ||
					Array.isArray(data.entries) ||
					'content' in data ||
					'size' in data
				)
					return data;
				return null;
			case 'system':
				return data.cpu ||
					data.memory ||
					data.os ||
					data.disks ||
					Array.isArray(data.displays) ||
					Array.isArray(data.variables) ||
					data.name ||
					'battery_percent' in data ||
					data.locked ||
					data.sleep ||
					data.hibernate
					? data
					: null;
			case 'process':
				return Array.isArray(data.processes) ? data : null;
			case 'window':
				return Array.isArray(data.windows) ||
					Array.isArray(data.elements) ||
					typeof data.text === 'string' ||
					data.waited === true
					? data
					: null;
			case 'actions':
				return Array.isArray(data.actions) ||
					typeof data.status === 'string' ||
					data.operation === 'result_injected'
					? data
					: null;
			case 'schedule':
				return Array.isArray(data.scheduled_actions) || (data.id && data.mode) ? data : null;
			case 'file':
				return data.written ||
					data.edited ||
					data.copied ||
					data.moved ||
					data.deleted ||
					Array.isArray(data.entries) ||
					'content' in data ||
					'size' in data
					? data
					: null;
			case 'http':
				return typeof data.status === 'number' ? data : null;
		case 'clipboard':
			return 'content' in data || data.written === true ? data : null;
		case 'web_search':
			// Provider built-in web search tool return: `{label, queries,
			// results:[{title,url,snippet}]}` composed by the page handler.
			return (Array.isArray(data.results) || Array.isArray(data.queries)) &&
				typeof data.label === 'string'
				? data
				: null;
			case 'agent':
				return data.operation ||
					data.ok === true ||
					data.ok === false ||
					Array.isArray(data.agents) ||
					data.session_id ||
					data.timed_out === true ||
					data.auto === true ||
					typeof data.text === 'string' ||
					data.reply ||
					data.message_id
					? data
					: null;
			default:
				return null;
		}
	}
</script>

<script>
	import { untrack } from 'svelte';
	import JsonView from '$lib/JsonView.svelte';
	import ContextMenu from '$lib/ContextMenu.svelte';
	import MaterialCollapsible from '$lib/MaterialCollapsible.svelte';
	import { getToolResultRenderer } from '$lib/toolResultRenderers.ts';
	import { copyText } from '$lib/clipboard.ts';
	import { actionStore, formatTokenCount, toolOutputPreviewStore } from '$lib/stores.ts';
	import {
		classifyToolSource,
		parseToolArgs,
		toolDisplayName,
		toolSourceLabel,
	} from '$lib/toolIdentity.ts';

	let {
		type = 'tool',
		toolName = '',
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
		usage = null,
		toolArgs = null,
	} = $props();

	let toolSource = $derived(classifyToolSource(toolName));
	let sourceBadge = $derived(toolSourceLabel(toolSource));
	let displayName = $derived(toolDisplayName(toolName));
	let hasToolArgs = $derived(toolArgs != null && toolArgs !== '');

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
		messageId ? /** @type {string|undefined} */ ($toolOutputPreviewStore[messageId]) : undefined,
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
				...(boundAction.error && !boundAction.output
					? { error: boundAction.error }
					: {}),
			});
		}
		if (livePreview != null && livePreview !== '') {
			return JSON.stringify({ output: livePreview });
		}
		return content;
	});

	let parsed = $derived(
		type === 'tool' ? parseToolResult(toolName, displayContent) : null,
	);

	// Collapsible body: expands while the tool streams so live output is
	// visible and auto-collapses once the observation is final (constraint
	// tool_call_output_expand_during_collapse_after). Only streaming
	// TRANSITIONS drive the state, so a manual click afterwards is never
	// clobbered by content-only re-renders. Background actions stay open
	// while `actionStore` reports running.
	let cardOpen = $state(untrack(() => liveStreaming));
	let lastStreaming = untrack(() => liveStreaming);
	$effect.pre(() => {
		if (liveStreaming === lastStreaming) return;
		cardOpen = liveStreaming;
		lastStreaming = liveStreaming;
	});
	let kind = $derived(parsed?.kind ?? null);
	let data = $derived(/** @type {any} */ (parsed?.data ?? {}));
	let BodyRenderer = $derived(getToolResultRenderer(kind, toolName, data));
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
			return [notifyParts.title, notifyParts.body].filter(Boolean).join('\n') || content || '';
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
	<div class="tool-card" role="status" oncontextmenu={handleContextMenu}>
		<div class="tool-card-header">
			<span class="tool-card-icon" aria-hidden="true">&#63;</span>
			<span class="tool-card-label">Haven 需要你确认</span>
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
	<div class="tool-card" role="status" oncontextmenu={handleContextMenu}>
		<MaterialCollapsible bind:open={cardOpen}>
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
				{:else if toolName === 'file_search' || toolName === 'files'}
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
						/><path d="M9 1v3M15 1v3M9 20v3M15 20v3M20 9h3M20 15h3M1 9h3M1 15h3" /></svg
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
		{:else if toolName === 'file' || (toolName === 'files' && !Array.isArray(data.results))}
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
			<span class="tool-card-label" title={toolName}>{displayName}</span>
			{#if usage}
				<span
					class="usage-chip"
					title={[
						usage.model ? `模型 ${usage.model}` : null,
						`上传 ${usage.prompt} → 生成 ${usage.completion} tokens`,
						usage.durationMs > 0 ? `耗时 ${(usage.durationMs / 1000).toFixed(1)}s` : null,
						usage.hasCost ? `费用 ${usage.cost.toFixed(6)} USD` : null,
						usage.cacheMiss > 0 ? `缓存未命中 ${formatTokenCount(usage.cacheMiss)}` : null,
						usage.cacheDiagnostics
							? `缓存策略 ${usage.cacheDiagnostics.mode || 'off'} / ${usage.cacheDiagnostics.outcome || 'unknown'}${usage.cacheDiagnostics.downgraded ? '（已兼容降级）' : ''}`
							: null,
						usage.calls > 1 ? `${usage.calls} 次调用合并` : null,
					]
						.filter(Boolean)
						.join('\n')}
				>
					{formatTokenCount(usage.total)} tokens
				</span>
			{/if}
			{/snippet}

		{#if cardOpen && hasToolArgs}
			{@const argsValue = parseToolArgs(toolArgs)}
			{#if argsValue != null}
				<div class="tool-args">
					<div class="tool-args-label">调用参数</div>
					<JsonView value={argsValue} defaultDepth={0} />
				</div>
			{/if}
		{/if}

		{#if BodyRenderer}
			<BodyRenderer
				kind={kind ?? undefined}
				data={data}
				shellText={shellText}
				liveStreaming={liveStreaming}
				rawText={rawText}
				parts={notifyParts}
			/>
		{:else if liveStreaming}
			<p class="tool-card-empty">等待输出…</p>
		{/if}
		{#if data.hint}
			<div class="tool-card-hint">{data.hint}</div>
		{/if}
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
			var(--md-sys-color-secondary-container) 45%,
			var(--md-sys-color-surface)
		);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		width: 420px;
		max-width: 100%;
		box-sizing: border-box;
		margin-top: var(--md-sys-space-xs);
	}
	.tool-card-header {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		margin-bottom: var(--md-sys-space-xs);
	}
	.tool-card-icon {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 20px;
		height: 20px;
		border-radius: 50%;
		background: var(--md-sys-color-secondary);
		color: var(--md-sys-color-on-secondary);
		font-size: 12px;
		font-weight: 700;
		flex: none;
	}
	.tool-card-label {
		font-size: 12px;
		font-weight: 700;
		color: var(--md-sys-color-on-secondary-container);
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.tool-source {
		flex: none;
		font-size: 10px;
		font-weight: 700;
		letter-spacing: 0.02em;
		line-height: 1.2;
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
	.tool-args {
		margin-bottom: var(--md-sys-space-sm);
		padding-bottom: var(--md-sys-space-sm);
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
	}
	.tool-args-label {
		font-size: 11px;
		font-weight: 600;
		color: var(--md-sys-color-on-surface-variant);
		margin-bottom: 4px;
	}
	.ask-question {
		margin: 0 0 var(--md-sys-space-sm);
		font-size: 13px;
		line-height: 1.5;
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
		font-size: 12px;
		font-weight: 600;
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
		font-size: 12px;
		cursor: pointer;
		transition: filter 0.15s ease;
	}
	.ask-ignore:hover {
		filter: brightness(0.9);
	}
	.ask-resolved {
		font-size: 12px;
		color: var(--md-sys-color-on-surface-variant);
	}
	.ask-waiting {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		font-size: 12px;
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
		font-size: 12px;
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
		font-size: 10px;
		font-weight: 600;
		font-family: var(--md-sys-typescale-mono);
		line-height: 1.6;
		white-space: nowrap;
		cursor: default;
	}
	.tool-card-hint {
		margin-top: var(--md-sys-space-xs);
		font-size: 10px;
		color: var(--md-sys-color-on-surface-variant);
		border-top: 1px dashed var(--md-sys-color-outline-variant);
		padding-top: var(--md-sys-space-xs);
	}
</style>
