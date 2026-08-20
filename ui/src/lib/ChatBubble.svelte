<script>
	import { onDestroy, untrack } from 'svelte';
	import { fly } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';
	import { imageDataUrl, formatTokenCount } from '$lib/stores.ts';
	import { getMarkdownRenderer, renderMarkdown } from '$lib/markdownRenderer.ts';
	import { handleExtRefEvent } from '$lib/externalRef.ts';
	import ToolResultCard from '$lib/ToolResultCard.svelte';

	let {
		role,
		content,
		type: msgType,
		time,
		voice = false,
		streaming = false,
		toolName = '',
		messageId = '',
		stepNumber = null,
		usage = null,
		attachments = [],
		options = [],
		awaiting = false,
		received = false,
		resolved = null,
		onContextMenu = null,
		onQuickReply = null,
		onIgnore = null,
	} = $props();

	// Local open state for the collapsible reasoning <details> block. The block
	// expands while streaming so live output is visible, and auto-collapses
	// once streaming ends (constraint
	// tool_call_output_expand_during_collapse_after). Manual clicks after that
	// persist: binding `open={streaming}` directly would re-apply the value on
	// every content-driven re-render, overriding a manual toggle.
	//
	// Tool result cards handle their own open state the same way inside
	// ToolResultCard, so every tool observation follows one rule.
	//
	// $effect.pre runs before the DOM updates, so the open/collapse happens
	// in the same frame as the streaming transition — no flash where the
	// block briefly renders open before snapping shut.
	let reasoningOpen = $state(untrack(() => streaming));
	let lastStreaming = untrack(() => streaming);
	$effect.pre(() => {
		// Only react to streaming TRANSITIONS, not to every re-render, so a
		// manual toggle is never clobbered.
		if (streaming === lastStreaming) return;
		if (streaming) {
			// Streaming (re)started → expand so live output is visible.
			reasoningOpen = true;
		} else {
			// Streaming ended → auto-collapse once.
			reasoningOpen = false;
		}
		lastStreaming = streaming;
	});

	let mdHtml = $state('');
	// L11: the component may be destroyed while onMount's dynamic imports are
	// still resolving; guard state writes against an unmounted component.
	let mounted = true;
	// Shared renderer resolution + per-frame streaming coalescing. Markdown is
	// rendered live while streaming so headings/bold/lists appear as they are
	// typed; code fences are deferred (plain <pre>) until streaming ends.
	let rendererReady = false;
	let rendererLoading = false;
	let mdRafId = 0;
	// Long-answer protection: the full accumulated text is re-parsed on every
	// render, and markdown-it re-renders grow linearly with the answer. A
	// frame-by-frame render (60/s) of a long answer saturates the webview main
	// thread, starves Tauri IPC, and makes streaming appear frozen. Cap live
	// previews to one render per MD_STREAM_RENDER_MS (~7/s — still visually
	// live); the final render when streaming ends is always immediate.
	const MD_STREAM_RENDER_MS = 150;
	let lastMdRender = 0;

	onDestroy(() => {
		mounted = false;
		if (mdRafId) cancelAnimationFrame(mdRafId);
	});

	/** @param {any} e */
	function handleContextMenu(e) {
		if (onContextMenu) {
			e.preventDefault();
			e.stopPropagation();
			let selectedContent = '';
			const selection = window.getSelection();
			if (selection && !selection.isCollapsed && selection.toString().trim()) {
				const el = e.currentTarget;
				if (el && el.contains(selection.anchorNode) && el.contains(selection.focusNode)) {
					selectedContent = selection.toString().trim();
				}
			}
			onContextMenu({
				x: e.clientX,
				y: e.clientY,
				messageId,
				stepNumber,
				role,
				content,
				type: msgType,
				selectedContent,
			});
		}
	}

	// Copy buttons inside rendered markdown code fences (md-code-copy), plus
	// `.ext-ref` URL/path links (click = copy, Ctrl+click = open). Click
	// delegation survives re-renders of {@html} content and works during
	// streaming. The whole text of the code block is copied, matching what is
	// highlighted, without any trailing newline.
	/** @param {any} e */
	function handleMdContentClick(e) {
		if (handleExtRefEvent(e)) return;
		const btn = e.target.closest?.('.md-code-copy');
		if (!btn) return;
		const wrap = btn.closest('.md-code-wrap');
		const codeEl = wrap?.querySelector('code');
		if (!codeEl) return;
		e.preventDefault();
		e.stopPropagation();
		const text = codeEl.textContent ?? '';
		navigator.clipboard
			?.writeText(text)
			.then(() => {
				const label = btn.querySelector('.md-code-copy-text');
				if (!label) return;
				const original = label.textContent;
				label.textContent = '已复制';
				setTimeout(() => {
					label.textContent = original;
				}, 1500);
			})
			.catch(() => {});
	}

	/** @param {any} e */
	function handleMdContentContextMenu(e) {
		handleExtRefEvent(e);
	}

	// `use:mdContent` attaches the delegation listeners to the rendered
	// markdown container. Wrapping them in an action (instead of `onclick` /
	// `onwheel` on the div) keeps the div non-interactive for a11y: only the
	// real copy buttons inside are clickable.
	//
	// Wide tables and code blocks get three affordances:
	//   1. Edge fade hints (--sh-l / --sh-r) that appear while the block is
	//      scrollable, refreshed on scroll, resize and content mutation. The
	//      fades are absolutely positioned on the NON-scrolling wrapper
	//      (.md-code-wrap / .md-table-wrap), so they stay fixed at the
	//      viewport edges while the content scrolls beneath them.
	//   2. Mouse wheel over a horizontally-scrollable block is translated to
	//      horizontal scrolling (when the block itself cannot scroll
	//      vertically), so mouse users don't need shift+wheel.
	//   3. A thin visible scrollbar, because scrollbars are hidden globally.
	// The CSS vars are written to the fade-hosting wrapper (or the element
	// itself for plain <pre> that never scrolls, e.g. streaming fences).
	/** @param {HTMLElement} el */
	function hintTarget(el) {
		const wrap = el.parentElement;
		if (wrap && (wrap.classList.contains('md-code-wrap') || wrap.classList.contains('md-table-wrap'))) {
			return wrap;
		}
		return el;
	}
	/** @param {any} el */
	function refreshScrollHint(el) {
		const target = hintTarget(el);
		const atLeft = el.scrollLeft <= 0;
		const atRight = el.scrollLeft + el.clientWidth >= el.scrollWidth - 1;
		target.style.setProperty('--sh-l', atLeft ? '0' : '1');
		target.style.setProperty('--sh-r', atRight ? '0' : '1');
	}

	/** @param {Event} e */
	function handleMdScrollCapture(e) {
		const el = e.target;
		if (el instanceof HTMLElement && (el.tagName === 'PRE' || el.tagName === 'TABLE')) {
			refreshScrollHint(el);
		}
	}

	/** @param {any} e */
	function handleMdWheel(e) {
		const el = e.target.closest?.('pre, table');
		if (!el) return;
		if (el.scrollWidth <= el.clientWidth + 1) return;
		if (el.scrollHeight > el.clientHeight + 1) return;
		if (Math.abs(e.deltaY) < Math.abs(e.deltaX)) return;
		e.preventDefault();
		el.scrollLeft += e.deltaY;
	}

	/** @param {HTMLElement} node */
	function mdContent(node) {
		let hintRaf = 0;
		function scheduleRefresh() {
			if (hintRaf) return;
			hintRaf = requestAnimationFrame(() => {
				hintRaf = 0;
				if (!mounted) return;
				node.querySelectorAll('pre, table').forEach(refreshScrollHint);
			});
		}
		node.addEventListener('click', handleMdContentClick);
		node.addEventListener('contextmenu', handleMdContentContextMenu);
		node.addEventListener('wheel', handleMdWheel, { passive: false });
		node.addEventListener('scroll', handleMdScrollCapture, true);
		const mo = new MutationObserver(scheduleRefresh);
		mo.observe(node, { childList: true, subtree: true });
		const ro = typeof ResizeObserver !== 'undefined' ? new ResizeObserver(scheduleRefresh) : null;
		ro?.observe(node);
		scheduleRefresh();
		return {
			destroy() {
				node.removeEventListener('click', handleMdContentClick);
				node.removeEventListener('contextmenu', handleMdContentContextMenu);
				node.removeEventListener('wheel', handleMdWheel);
				node.removeEventListener('scroll', handleMdScrollCapture, true);
				mo.disconnect();
				ro?.disconnect();
				if (hintRaf) cancelAnimationFrame(hintRaf);
			},
		};
	}

	// L11: guard the render effect against unmount mid-import. mdHtml stays ''
	// until the shared renderer is loaded and this bubble is still mounted.
	// Only assistant text bubbles render markdown; everything else (user,
	// thought, reasoning, tool, ask, supplement) skips the shared instance
	// entirely. While streaming, re-render on every content change (coalesced
	// to one render per animation frame) so markdown appears live; the renderer
	// defers code blocks to the final render.
	$effect(() => {
		if (!mounted || role !== 'assistant' || msgType) return;
		if (!rendererReady) {
			// Renderer still loading — show plain text with the caret, then
			// render once the shared instance resolves.
			if (!rendererLoading) {
				rendererLoading = true;
				getMarkdownRenderer().then(() => {
					if (!mounted) return;
					rendererReady = true;
					renderNow();
				});
			}
			mdHtml = '';
			return;
		}
		if (streaming) {
			// Coalesce chunk updates: at most one markdown render per frame
			// (rAF) and at most one per MD_STREAM_RENDER_MS (time throttle).
			// A skipped render retries next frame instead of being dropped so
			// the preview never lags more than one window behind the text.
			if (mdRafId) return;
			const tryRender = () => {
				if (!mounted) return;
				const now = performance.now();
				if (now - lastMdRender < MD_STREAM_RENDER_MS) {
					mdRafId = requestAnimationFrame(tryRender);
					return;
				}
				mdRafId = 0;
				renderNow();
			};
			mdRafId = requestAnimationFrame(tryRender);
			return;
		}
		if (mdRafId) {
			cancelAnimationFrame(mdRafId);
			mdRafId = 0;
		}
		renderNow();
	});

	// Reads the current props, so it is safe to call from the rAF callback
	// and from the renderer-load completion.
	function renderNow() {
		const text = content || '';
		mdHtml = text ? renderMarkdown(text, !!streaming) : '';
		lastMdRender = performance.now();
	}
</script>

<div
	class="bubble"
	class:user={role === 'user'}
	class:assistant={role === 'assistant'}
	class:streaming
	role="button"
	tabindex="0"
	oncontextmenu={handleContextMenu}
	in:fly={{ y: 4, duration: 300, easing: cubicOut }}
>
	<div class="bubble-header">
		<span class="bubble-role">
			{role === 'user' ? 'You' : 'Haven'}
			{#if voice}<span class="mic-icon" title="Voice input">&#127908;</span>{/if}
			{#if role === 'user' && received}<span class="received-tag" title="Agent 已收到">✓</span
				>{/if}
		</span>
		{#if time}
			<span class="bubble-time">{time}</span>
		{/if}
	</div>
	<div class="bubble-content">
		{#if msgType === 'thought'}
			<em class="thought"
				>{content}{#if streaming && content}<span class="caret"></span>{/if}</em
			>
		{:else if msgType === 'reasoning'}
			<details class="reasoning-block" bind:open={reasoningOpen}>
				<summary class="reasoning-summary">Thinking...</summary>
				<div class="reasoning-content">
					<em
						>{content}{#if streaming && content}<span class="caret"></span>{/if}</em
					>
				</div>
			</details>
		{:else if msgType === 'tool'}
			<div class="tool-call-row">
				<span class="tool-call">&#9654; Calling {toolName}</span>
				{#if usage}
					<span
						class="usage-chip"
						title={[
							usage.model ? `模型 ${usage.model}` : null,
							`上传 ${usage.prompt} → 生成 ${usage.completion} tokens`,
							usage.durationMs > 0 ? `耗时 ${(usage.durationMs / 1000).toFixed(1)}s` : null,
							usage.hasCost ? `费用 ${usage.cost.toFixed(6)} USD` : null,
							usage.calls > 1 ? `${usage.calls} 次调用合并` : null,
						].filter(Boolean).join('\n')}
					>
						{formatTokenCount(usage.total)} tokens
					</span>
				{/if}
			</div>
			{#if content}
				<ToolResultCard {toolName} {content} {streaming} />
			{/if}
		{:else if msgType === 'ask'}
			<ToolResultCard
				type="ask"
				{content}
				{options}
				{awaiting}
				{messageId}
				{resolved}
				{onQuickReply}
				{onIgnore}
			/>
		{:else if msgType === 'supplement'}
			<div class="supplement-badge">&#10100; {content}</div>
		{:else if role === 'assistant'}
			{#if mdHtml}
				<div class="md-content" class:streaming use:mdContent>
					{@html mdHtml}{#if streaming && content}<span class="caret"></span>{/if}
				</div>
			{:else}
				<p>
					{content}{#if streaming && content}<span class="caret"></span>{/if}
				</p>
			{/if}
		{:else}
			{#if attachments && attachments.length > 0}
				<div class="attachments">
					{#each attachments as att}
						{#if (att.media_type || '').startsWith('image/') && att.data}
							<img
								class="attachment-img"
								src={imageDataUrl(att)}
								alt="用户发送的图片"
								loading="lazy"
							/>
						{:else if (att.media_type || '').startsWith('audio/') && att.data}
							<audio class="attachment-audio" controls preload="none" src={imageDataUrl(att)} title={att.filename || '语音'}>
								你的浏览器不支持音频播放
							</audio>
						{:else}
							<div class="attachment-file" title={att.path || att.filename || '附件'}>
								<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
									<path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
									<polyline points="14 2 14 8 20 8" />
								</svg>
								<span class="attachment-file-name">{att.filename || att.path || '附件'}</span>
							</div>
						{/if}
					{/each}
				</div>
			{/if}
			{#if content}
				<p>{content}</p>
			{/if}
		{/if}
	</div>
</div>

<style>
	.bubble {
		max-width: 85%;
		min-width: 35%;
		width: fit-content;
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border-radius: var(--md-sys-shape-large);
		font-size: 13px;
		line-height: 1.5;
	}
	.bubble.user {
		margin-left: auto;
		background: color-mix(
			in srgb,
			var(--md-sys-color-primary) 78%,
			var(--md-sys-color-surface)
		);
		color: var(--md-sys-color-on-primary);
		border: none;
		border-radius: var(--md-sys-shape-large) var(--md-sys-shape-large)
			var(--md-sys-shape-extra-small) var(--md-sys-shape-large);
	}
	.bubble.assistant {
		margin-right: auto;
		background: color-mix(
			in srgb,
			var(--md-sys-color-primary-container) 20%,
			var(--md-sys-color-surface)
		);
		color: var(--md-sys-color-on-primary-container);
		border: none;
		border-radius: var(--md-sys-shape-large) var(--md-sys-shape-large) var(--md-sys-shape-large)
			var(--md-sys-shape-extra-small);
	}
	.bubble-header {
		display: flex;
		justify-content: space-between;
		margin-bottom: var(--md-sys-space-2xs);
	}
	.bubble-role {
		font-size: 11px;
		font-weight: 700;
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
	}
	.mic-icon {
		font-size: 12px;
		filter: grayscale(0.3);
	}
	.received-tag {
		font-size: 11px;
		line-height: 1;
		color: color-mix(in srgb, var(--md-sys-color-on-primary) 80%, var(--md-sys-color-primary));
	}
	.bubble-time {
		font-size: 10px;
	}
	.bubble.user .bubble-time {
		color: color-mix(in srgb, var(--md-sys-color-on-primary) 95%, var(--md-sys-color-primary));
	}
	.bubble.assistant .bubble-time {
		color: var(--md-sys-color-on-surface-variant);
		opacity: 0.7;
	}
	.thought {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 12px;
		font-style: italic;
	}
	.caret {
		display: inline-block;
		width: 6px;
		height: 12px;
		margin-left: 2px;
		background: currentColor;
		animation: blink 1s step-end infinite;
		vertical-align: middle;
	}
	@keyframes blink {
		50% {
			background: transparent;
		}
	}
	.tool-call {
		background: var(--md-sys-color-tertiary-container);
		color: var(--md-sys-color-on-tertiary-container);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border-radius: var(--md-sys-shape-small);
		font-size: 12px;
		font-weight: 600;
		display: inline-block;
		margin-bottom: var(--md-sys-space-xs);
	}
	.tool-call-row {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		flex-wrap: wrap;
		margin-bottom: var(--md-sys-space-xs);
	}
	.usage-chip {
		display: inline-block;
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
	.supplement-badge {
		background: var(--md-sys-color-warning-container);
		color: var(--md-sys-color-on-warning-container);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border-radius: var(--md-sys-shape-small);
		font-size: 12px;
		font-weight: 600;
		display: inline-block;
	}
	.attachments {
		display: flex;
		flex-wrap: wrap;
		gap: var(--md-sys-space-xs);
		margin-bottom: var(--md-sys-space-xs);
	}
	.attachment-img {
		max-width: 240px;
		max-height: 180px;
		border-radius: var(--md-sys-shape-small);
		border: 1px solid color-mix(in srgb, var(--md-sys-color-on-primary) 25%, transparent);
		object-fit: contain;
		display: block;
		cursor: zoom-in;
	}
	.attachment-img:hover {
		opacity: 0.9;
	}
	.attachment-audio {
		max-width: 240px;
		height: 36px;
		border-radius: var(--md-sys-shape-small);
		border: 1px solid color-mix(in srgb, var(--md-sys-color-on-primary) 25%, transparent);
	}
	.attachment-file {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		max-width: 220px;
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		border: 1px solid color-mix(in srgb, var(--md-sys-color-on-primary) 25%, transparent);
		background: color-mix(in srgb, var(--md-sys-color-on-primary) 8%, transparent);
		font-size: 12px;
		color: var(--md-sys-color-on-surface);
	}
	.attachment-file-name {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.md-content :global(p) {
		margin: 0 0 0.75em;
	}
	.md-content :global(p:last-child) {
		margin-bottom: 0;
	}
	.md-content :global(pre) {
		position: relative;
		background: var(--md-sys-color-surface-container-high);
		padding: var(--md-sys-space-md);
		border-radius: var(--md-sys-shape-small);
		font-size: 12px;
		overflow-x: auto;
		margin: 0 0 0.75em;
		scrollbar-width: thin;
		scrollbar-color: var(--md-sys-color-outline-variant) transparent;
	}
	.md-content :global(code) {
		font-family: var(--md-sys-typescale-mono);
		font-size: 11px;
		background: color-mix(in srgb, var(--md-sys-color-on-surface) 8%, transparent);
		padding: 1px 4px;
		border-radius: 3px;
	}
	.md-content :global(pre code) {
		background: none;
		padding: 0;
		font-size: 12px;
	}
	.md-content :global(pre.md-code-streaming) {
		white-space: pre-wrap;
		word-break: break-word;
	}
	.md-content :global(.md-code-wrap) {
		position: relative;
		background: var(--md-sys-color-surface-container-high);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		overflow: hidden;
		margin: 0 0 0.75em;
	}
	.md-content :global(.md-code-wrap pre) {
		background: none;
		margin: 0;
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border-radius: 0;
	}
	.md-content :global(.md-table-wrap) {
		position: relative;
		overflow: hidden;
		border-radius: var(--md-sys-shape-small);
		margin: 0 0 0.75em;
	}
	.md-content :global(.md-table-wrap table) {
		margin: 0;
		border-radius: 0;
	}
	.md-content :global(.md-code-bar) {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 2px var(--md-sys-space-xs);
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
	}
	.md-content :global(.md-code-lang) {
		font-size: 10px;
		font-weight: 600;
		font-family: var(--md-sys-typescale-mono);
		color: var(--md-sys-color-on-surface-variant);
		text-transform: uppercase;
	}
	.md-content :global(.md-code-copy) {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		font-size: 10px;
		font-weight: 600;
		color: var(--md-sys-color-on-surface-variant);
		background: transparent;
		border: 1px solid transparent;
		border-radius: var(--md-sys-shape-full);
		padding: 1px 8px;
		cursor: pointer;
		transition:
			background-color 0.15s ease,
			color 0.15s ease,
			border-color 0.15s ease;
	}
	.md-content :global(.md-code-copy:hover) {
		background: var(--md-sys-color-surface-container-highest);
		color: var(--md-sys-color-on-surface);
	}
	.md-content :global(.md-code-copy svg) {
		flex: none;
	}
	.md-content :global(.hljs-keyword) {
		color: var(--md-sys-color-primary);
	}
	.md-content :global(.hljs-string) {
		color: var(--md-sys-color-success);
	}
	.md-content :global(.hljs-number) {
		color: var(--md-sys-color-tertiary);
	}
	.md-content :global(.hljs-comment) {
		color: var(--md-sys-color-on-surface-variant);
		font-style: italic;
		opacity: 0.7;
	}
	.md-content :global(.hljs-function) {
		color: var(--md-sys-color-primary);
	}
	.md-content :global(.hljs-title) {
		color: var(--md-sys-color-primary);
	}
	.md-content :global(.hljs-params) {
		color: var(--md-sys-color-on-surface);
	}
	.md-content :global(.hljs-built_in) {
		color: var(--md-sys-color-tertiary);
	}
	.md-content :global(.hljs-type) {
		color: color-mix(in srgb, var(--md-sys-color-tertiary) 80%, var(--md-sys-color-primary));
	}
	.md-content :global(.hljs-literal) {
		color: var(--md-sys-color-primary);
	}
	.md-content :global(.hljs-selector-class) {
		color: var(--md-sys-color-tertiary);
	}
	.md-content :global(.hljs-title.class_) {
		color: var(--md-sys-color-tertiary);
	}
	.md-content :global(.hljs-selector-tag) {
		color: var(--md-sys-color-primary);
	}
	.md-content :global(.hljs-attr) {
		color: var(--md-sys-color-tertiary);
	}
	.md-content :global(.hljs-attribute) {
		color: var(--md-sys-color-tertiary);
	}
	.md-content :global(.hljs-variable) {
		color: var(--md-sys-color-error);
	}
	.md-content :global(.hljs-meta) {
		color: var(--md-sys-color-on-surface-variant);
		opacity: 0.7;
	}
	.md-content :global(.hljs-property) {
		color: var(--md-sys-color-on-surface);
	}
	.md-content :global(.hljs-punctuation) {
		color: var(--md-sys-color-on-surface-variant);
	}
	.md-content :global(.hljs-operator) {
		color: var(--md-sys-color-on-surface-variant);
	}
	.md-content :global(ul),
	.md-content :global(ol) {
		padding-left: 1.5em;
		margin: 0 0 0.75em;
	}
	.md-content :global(li) {
		margin-bottom: 0.25em;
	}
	.md-content :global(blockquote) {
		border-left: 3px solid var(--md-sys-color-primary);
		margin: 0 0 0.75em;
		padding: var(--md-sys-space-xs) var(--md-sys-space-md);
		background: color-mix(in srgb, var(--md-sys-color-primary) 8%, transparent);
		border-radius: 0 var(--md-sys-shape-extra-small) var(--md-sys-shape-extra-small) 0;
		color: var(--md-sys-color-on-surface-variant);
	}
	.md-content :global(hr) {
		border: none;
		border-top: 1px solid var(--md-sys-color-outline-variant);
		margin: 0.75em 0;
	}
	.md-content :global(table) {
		position: relative;
		border-collapse: collapse;
		display: block;
		overflow-x: auto;
		width: 100%;
		margin: 0 0 0.75em;
		font-size: 12px;
		scrollbar-width: thin;
		scrollbar-color: var(--md-sys-color-outline-variant) transparent;
	}	.md-content :global(th),
	.md-content :global(td) {
		border: 1px solid var(--md-sys-color-outline-variant);
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		text-align: left;
	}
	.md-content :global(th) {
		background: var(--md-sys-color-surface-container-high);
		font-weight: 600;
	}
	/* Wide content affordances (scrollbars are hidden globally, so pre/table
	 * re-enable a slim one and get edge fade hints driven by JS: --sh-l and
	 * --sh-r are 1 while content is clipped on that side). The fades are
	 * pseudo-elements of the NON-scrolling wrappers (.md-code-wrap /
	 * .md-table-wrap), so they stay pinned to the viewport edges while the
	 * inner pre/table scrolls. */
	.md-content :global(pre)::-webkit-scrollbar,
	.md-content :global(table)::-webkit-scrollbar {
		display: block;
		height: 4px;
	}
	.md-content :global(pre)::-webkit-scrollbar-track,
	.md-content :global(table)::-webkit-scrollbar-track {
		background: transparent;
	}
	.md-content :global(pre)::-webkit-scrollbar-thumb,
	.md-content :global(table)::-webkit-scrollbar-thumb {
		background: var(--md-sys-color-outline-variant);
		border-radius: 2px;
	}
	.md-content :global(.md-code-wrap)::before,
	.md-content :global(.md-code-wrap)::after,
	.md-content :global(.md-table-wrap)::before,
	.md-content :global(.md-table-wrap)::after {
		content: '';
		position: absolute;
		top: 0;
		bottom: 0;
		width: 14px;
		z-index: 1;
		pointer-events: none;
		opacity: 0;
		transition: opacity 0.15s ease;
	}
	.md-content :global(.md-code-wrap)::before,
	.md-content :global(.md-table-wrap)::before {
		left: 0;
		opacity: var(--sh-l, 0);
		background: linear-gradient(to right, var(--md-sys-color-surface-container-high), transparent);
		border-radius: var(--md-sys-shape-small) 0 var(--md-sys-shape-small) 0;
	}
	.md-content :global(.md-code-wrap)::after,
	.md-content :global(.md-table-wrap)::after {
		right: 0;
		opacity: var(--sh-r, 0);
		background: linear-gradient(to left, var(--md-sys-color-surface-container-high), transparent);
		border-radius: 0 var(--md-sys-shape-small) 0 var(--md-sys-shape-small);
	}
	.md-content :global(.md-table-wrap)::before {
		background: linear-gradient(
			to right,
			color-mix(in srgb, var(--md-sys-color-primary-container) 20%, var(--md-sys-color-surface)),
			transparent
		);
		border-radius: 0;
	}
	.md-content :global(.md-table-wrap)::after {
		background: linear-gradient(
			to left,
			color-mix(in srgb, var(--md-sys-color-primary-container) 20%, var(--md-sys-color-surface)),
			transparent
		);
		border-radius: 0;
	}
	.md-content :global(.md-code-bar) {
		position: relative;
		z-index: 2;
	}
	.md-content :global(strong) {
		font-weight: 700;
	}
	.md-content :global(a),
	.md-content :global(.ext-ref) {
		color: var(--md-sys-color-primary);
		text-decoration: underline;
		text-underline-offset: 2px;
		cursor: pointer;
		word-break: break-all;
	}
	.md-content :global(.ext-ref:hover) {
		color: color-mix(in srgb, var(--md-sys-color-primary) 80%, var(--md-sys-color-on-surface));
	}
	.md-content :global(.ext-ref-path) {
		font-family: var(--md-sys-typescale-mono);
		font-size: 0.95em;
	}
	.md-content :global(h1),
	.md-content :global(h2),
	.md-content :global(h3),
	.md-content :global(h4) {
		font-weight: 600;
		margin: 0 0 0.5em;
		color: var(--md-sys-color-on-surface);
	}
	.md-content :global(h1) {
		font-size: 17px;
	}
	.md-content :global(h2) {
		font-size: 15px;
	}
	.md-content :global(h3) {
		font-size: 14px;
	}
	.reasoning-block {
		background: color-mix(in srgb, var(--md-sys-color-primary) 6%, transparent);
		border-radius: var(--md-sys-shape-small);
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		font-size: 12px;
	}
	.reasoning-summary {
		color: var(--md-sys-color-primary);
		font-weight: 600;
		cursor: pointer;
		font-size: 11px;
		user-select: none;
	}
	.reasoning-content {
		margin-top: var(--md-sys-space-xs);
		color: var(--md-sys-color-on-surface-variant);
	}
	.reasoning-content :global(em) {
		font-style: italic;
	}
</style>
