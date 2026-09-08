<script>
	import { onDestroy, untrack } from 'svelte';
	import { fly } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';
	import { imageDataUrl } from '$lib/stores.ts';
	import { getMarkdownRenderer, renderMarkdown } from '$lib/markdownRenderer.ts';
	import { handleExtRefEvent } from '$lib/externalRef.ts';
	import { createDragScrollController } from '$lib/dragScroll.ts';
	import logger from '$lib/logger.ts';
	import { formatError } from '$lib/formatError.ts';
	import { PEER_KICKOFF_PREFIX } from '$lib/peerKickoff.ts';
	import ToolResultCard from '$lib/ToolResultCard.svelte';
	import MaterialCollapsible from '$lib/MaterialCollapsible.svelte';

	let {
		role,
		content,
		type: msgType,
		time,
		voice = false,
		streaming = false,
		toolName = '',
		unrecoverable = false,
		outcome = null,
		messageId = '',
		stepNumber = null,
		toolArgs = null,
		attachments = [],
		options = [],
		awaiting = false,
		received = false,
		resolved = null,
		actionId = null,
		showFallbackIntent = false,
		onContextMenu = null,
		onAskSelectionChange = null,
		onIgnore = null,
		onAskSubmit = null,
	} = $props();

	// Local open state for the collapsible reasoning block. The block
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
	// Shared renderer resolution. Markdown/highlighting is intentionally kept
	// off the hot streaming path: a lightweight text preview paints every
	// chunk immediately, then the completed answer is upgraded to Markdown.
	let rendererReady = false;
	let rendererLoading = false;

	onDestroy(() => {
		mounted = false;
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
			.catch((error) => {
				logger.warn('ChatBubble', 'markdown copy failed', formatError(error));
			});
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
		if (
			wrap &&
			(wrap.classList.contains('md-code-wrap') || wrap.classList.contains('md-table-wrap'))
		) {
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
		const dragController = createDragScrollController(node, {
			axis: 'x',
			resolveTarget(target) {
				const element = target instanceof Element ? target.closest('pre, table') : null;
				return element instanceof HTMLElement ? element : null;
			},
		});
		function scheduleRefresh() {
			// Skip edge-fade updates while streaming: content mutates every
			// frame and toggling --sh-l/--sh-r causes visible edge flicker.
			if (node.classList.contains('streaming')) return;
			if (hintRaf) return;
			hintRaf = requestAnimationFrame(() => {
				hintRaf = 0;
				if (!mounted) return;
				if (node.classList.contains('streaming')) return;
				node.querySelectorAll('pre, table').forEach(refreshScrollHint);
			});
		}
		node.addEventListener('click', handleMdContentClick);
		node.addEventListener('contextmenu', handleMdContentContextMenu);
		node.addEventListener('wheel', handleMdWheel, { passive: false });
		node.addEventListener('scroll', handleMdScrollCapture, true);
		const mo = new MutationObserver(scheduleRefresh);
		// Watch class too: when `.streaming` is removed, recompute edge fades
		// once for the final layout (content may not mutate again).
		mo.observe(node, {
			childList: true,
			subtree: true,
			attributes: true,
			attributeFilter: ['class'],
		});
		const ro =
			typeof ResizeObserver !== 'undefined' ? new ResizeObserver(scheduleRefresh) : null;
		ro?.observe(node);
		scheduleRefresh();
		return {
			destroy() {
				node.removeEventListener('click', handleMdContentClick);
				node.removeEventListener('contextmenu', handleMdContentContextMenu);
				node.removeEventListener('wheel', handleMdWheel);
				node.removeEventListener('scroll', handleMdScrollCapture, true);
				dragController.destroy();
				mo.disconnect();
				ro?.disconnect();
				if (hintRaf) cancelAnimationFrame(hintRaf);
			},
		};
	}

	let isPeerKickoff = $derived(
		msgType === 'peer_kickoff' ||
			(typeof content === 'string' && content.startsWith(PEER_KICKOFF_PREFIX)),
	);
	// Keep the Markdown effect aligned with the template branch below. Some
	// persisted messages carry a non-text type that has no dedicated bubble;
	// they still represent assistant content and must render code fences.
	let rendersMarkdown = $derived(
		role === 'assistant' &&
			!isPeerKickoff &&
			!['thought', 'reasoning', 'tool', 'ask', 'supplement'].includes(msgType),
	);

	// L11: guard the render effect against unmount mid-import. mdHtml stays ''
	// until the shared renderer is loaded and this bubble is still mounted.
	// Only assistant text bubbles render markdown; everything else (user,
	// thought, reasoning, tool, ask, supplement) skips the shared instance
	// entirely. Streaming assistant text stays as a cheap escaped text preview
	// so the UI cannot fall behind while markdown/highlighting reparses a long
	// answer. The final state is rendered with the shared Markdown instance.
	$effect(() => {
		if (!mounted || !rendersMarkdown || streaming) return;
		if (!rendererReady) {
			// Renderer still loading — show plain text with the caret, then
			// render once the shared instance resolves.
			if (!rendererLoading) {
				rendererLoading = true;
				getMarkdownRenderer().then(() => {
					if (!mounted) return;
					rendererReady = true;
					if (!streaming) renderNow();
				});
			}
			mdHtml = '';
			return;
		}
		renderNow();
	});

	// Reads the current props, so it is safe to call from the renderer-load
	// completion.
	function renderNow() {
		const text = content || '';
		mdHtml = text ? renderMarkdown(text) : '';
	}
</script>

<div
	class="bubble"
	class:user={role === 'user' && !isPeerKickoff}
	class:assistant={role === 'assistant' || isPeerKickoff}
	class:thinking={msgType === 'thought' || msgType === 'reasoning'}
	class:tool={msgType === 'tool' || msgType === 'ask'}
	class:streaming
	role="article"
	oncontextmenu={handleContextMenu}
	in:fly={{ y: 4, duration: 300, easing: cubicOut }}
>
	{#if msgType !== 'tool' && msgType !== 'ask'}
		<div class="bubble-header">
			<span class="bubble-role">
				{#if isPeerKickoff}
					Peer 委托
				{:else if role === 'user'}
					You
				{:else}
					Haven
				{/if}
				{#if voice}<span class="mic-icon" title="Voice input">&#127908;</span>{/if}
				{#if role === 'user' && !isPeerKickoff && received}<span
						class="received-tag"
						title="Agent 已收到">✓</span
					>{/if}
			</span>
			{#if time}
				<span class="bubble-time">{time}</span>
			{/if}
		</div>
	{/if}
	<div class="bubble-content">
		{#if isPeerKickoff}
			<div class="peer-kickoff-badge" title="低信任委托任务，不是用户指令">
				<span class="peer-kickoff-label">低信任委托</span>
				<pre class="peer-kickoff-body">{content}</pre>
			</div>
		{:else if msgType === 'thought'}
			<span class="thought"
				>{content}{#if streaming && content}<span class="caret"></span>{/if}</span
			>
		{:else if msgType === 'reasoning'}
			<div class="reasoning-block">
				<MaterialCollapsible bind:open={reasoningOpen} lazy>
					{#snippet header()}
						<span class="reasoning-summary">Thinking...</span>
					{/snippet}
					<div class="reasoning-content">
						<span
							>{content}{#if streaming && content}<span class="caret"
								></span>{/if}</span
						>
					</div>
				</MaterialCollapsible>
			</div>
		{:else if msgType === 'tool'}
			<ToolResultCard
				embedded
				{toolName}
				{unrecoverable}
				{outcome}
				{content}
				{streaming}
				{actionId}
				{toolArgs}
				{showFallbackIntent}
				{messageId}
			/>
		{:else if msgType === 'ask'}
			<ToolResultCard
				embedded
				type="ask"
				{content}
				{options}
				{awaiting}
				{messageId}
				{resolved}
				{onAskSelectionChange}
				{onIgnore}
				{onAskSubmit}
			/>
		{:else if msgType === 'supplement'}
			<div class="supplement-badge">&#10100; {content}</div>
		{:else if rendersMarkdown}
			{#if streaming}
				<p class="streaming-preview">
					{content}{#if content}<span class="caret"></span>{/if}
				</p>
			{:else if mdHtml}
				<div class="md-content" class:streaming use:mdContent>
					{@html mdHtml}
				</div>
			{:else}
				<p>{content}</p>
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
							<audio
								class="attachment-audio"
								controls
								preload="none"
								src={imageDataUrl(att)}
								title={att.filename || '语音'}
							>
								你的浏览器不支持音频播放
							</audio>
						{:else}
							<div class="attachment-file" title={att.path || att.filename || '附件'}>
								<svg
									width="14"
									height="14"
									viewBox="0 0 24 24"
									fill="none"
									stroke="currentColor"
									stroke-width="2"
									stroke-linecap="round"
									stroke-linejoin="round"
									aria-hidden="true"
								>
									<path
										d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"
									/>
									<polyline points="14 2 14 8 20 8" />
								</svg>
								<span class="attachment-file-name"
									>{att.filename || att.path || '附件'}</span
								>
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
		width: 100%;
		max-width: 100%;
		min-width: 0;
		min-inline-size: 0;
		box-sizing: border-box;
		padding: var(--md-sys-space-md) var(--md-sys-space-lg);
		border-radius: var(--md-sys-shape-large);
		font-size: var(--md-sys-typescale-body-medium-size);
		line-height: var(--md-sys-typescale-body-medium-line-height);
		overflow-wrap: anywhere;
		transition:
			background-color var(--md-sys-motion-duration-short)
				var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			box-shadow var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	/* The global long-conversation optimization may skip off-screen bubbles;
	 * the active stream is the exception because its newest text must paint
	 * immediately and remain available to the auto-follow scroll boundary. */
	.bubble.streaming {
		content-visibility: visible;
	}
	.bubble.thinking {
		width: 100%;
		max-width: 100%;
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		background: transparent;
		border-color: transparent;
		box-shadow: none;
	}
	.bubble.tool {
		width: 100%;
		max-width: 100%;
		/* Tool calls use the same outer surface as assistant messages. The
		 * nested result component only owns the header and details. */
	}
	.bubble.user {
		margin-left: auto;
		width: var(--md-sys-chat-user-max-width);
		max-width: var(--md-sys-chat-user-max-width);
		background: color-mix(
			in srgb,
			var(--md-sys-color-primary) 84%,
			var(--md-sys-color-surface)
		);
		color: var(--md-sys-color-on-primary);
		border: 1px solid color-mix(in srgb, var(--md-sys-color-primary) 55%, transparent);
		border-radius: var(--md-sys-shape-large) var(--md-sys-shape-large)
			var(--md-sys-shape-extra-small) var(--md-sys-shape-large);
		box-shadow: var(--md-sys-elevation-1);
	}
	.bubble.assistant,
	.bubble.tool.assistant {
		margin-right: auto;
		width: 100%;
		max-width: 100%;
		background: color-mix(
			in srgb,
			var(--md-sys-color-primary-container) 18%,
			var(--md-sys-color-surface-container-low)
		);
		color: var(--md-sys-color-on-surface);
		border: 1px solid
			color-mix(in srgb, var(--md-sys-color-primary) 18%, var(--md-sys-color-outline-variant));
		border-left: 3px solid color-mix(in srgb, var(--md-sys-color-primary) 72%, transparent);
		border-radius: var(--md-sys-shape-large) var(--md-sys-shape-large) var(--md-sys-shape-large)
			var(--md-sys-shape-extra-small);
		box-shadow: var(--md-sys-elevation-1);
	}
	.bubble.assistant.thinking {
		background: transparent;
		border-color: transparent;
		box-shadow: none;
	}
	.bubble.user .bubble-role {
		color: color-mix(in srgb, var(--md-sys-color-on-primary) 88%, var(--md-sys-color-primary));
	}
	.bubble.assistant .bubble-role {
		color: var(--md-sys-color-primary);
	}
	.bubble-content {
		min-width: 0;
		width: 100%;
	}
	.bubble-header {
		display: flex;
		justify-content: space-between;
		align-items: center;
		gap: var(--md-sys-space-md);
		margin-bottom: var(--md-sys-space-sm);
	}
	.bubble-role {
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-label-small-line-height);
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
	}
	.bubble-role::before {
		content: '';
		width: var(--md-sys-space-sm);
		height: var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-full);
		background: currentColor;
		opacity: 0.78;
		flex: none;
	}
	.mic-icon {
		font-size: 12px;
		filter: grayscale(0.3);
	}
	.received-tag {
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: color-mix(in srgb, var(--md-sys-color-on-primary) 80%, var(--md-sys-color-primary));
	}
	.bubble.user .received-tag {
		color: color-mix(in srgb, var(--md-sys-color-on-primary) 82%, var(--md-sys-color-primary));
	}
	.bubble-time {
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
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
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
		font-style: italic;
		opacity: 0.88;
	}
	.bubble-content > p {
		margin: 0;
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}
	.streaming-preview {
		margin: 0;
		white-space: pre-wrap;
		overflow-wrap: anywhere;
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
	.supplement-badge {
		background: var(--md-sys-color-warning-container);
		color: var(--md-sys-color-on-warning-container);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border-radius: var(--md-sys-shape-small);
		font-size: var(--md-sys-typescale-label-medium-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-medium-line-height);
		display: inline-block;
	}
	.peer-kickoff-badge {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
		background: color-mix(in srgb, var(--md-sys-color-tertiary-container) 55%, transparent);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
	}
	.peer-kickoff-label {
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 700;
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-tertiary-container);
		letter-spacing: var(--md-sys-typescale-overline-letter-spacing);
	}
	.peer-kickoff-body {
		margin: 0;
		white-space: pre-wrap;
		word-break: break-word;
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-code-size);
		line-height: var(--md-sys-typescale-code-line-height);
		color: var(--md-sys-color-on-surface-variant);
		max-height: 12em;
		overflow: auto;
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
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface);
	}
	.attachment-file-name {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.md-content :global(p) {
		margin: 0 0 0.75em;
		overflow-wrap: anywhere;
		line-height: var(--md-sys-typescale-body-medium-line-height);
	}
	.md-content :global(p:last-child) {
		margin-bottom: 0;
	}
	.md-content :global(pre) {
		position: relative;
		background: var(--md-sys-color-surface-container-high);
		padding: var(--md-sys-space-md);
		border-radius: var(--md-sys-shape-small);
		font-size: var(--md-sys-typescale-code-size);
		line-height: var(--md-sys-typescale-code-line-height);
		overflow-x: auto;
		cursor: grab;
		touch-action: pan-x;
		margin: 0 0 0.75em;
		scrollbar-width: thin;
		scrollbar-color: var(--md-sys-color-outline-variant) transparent;
	}
	.md-content :global(code) {
		font-family: var(--md-sys-typescale-mono);
		font-size: var(--md-sys-typescale-code-size);
		line-height: var(--md-sys-typescale-code-line-height);
		background: color-mix(in srgb, var(--md-sys-color-on-surface) 8%, transparent);
		padding: 1px 4px;
		border-radius: 3px;
	}
	.md-content :global(pre code) {
		background: none;
		padding: 0;
		font-size: var(--md-sys-typescale-code-size);
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
		background: var(--md-sys-color-surface);
		margin: 0 0 0.75em;
	}
	.md-content :global(.md-table-wrap > .md-table) {
		margin: 0;
		border-radius: inherit;
	}
	.md-content :global(.md-code-bar) {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 2px var(--md-sys-space-xs);
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
	}
	.md-content :global(.md-code-lang) {
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-small-line-height);
		font-family: var(--md-sys-typescale-mono);
		color: var(--md-sys-color-on-surface-variant);
		text-transform: uppercase;
		letter-spacing: var(--md-sys-typescale-overline-letter-spacing);
	}
	.md-content :global(.md-code-copy) {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		font-size: var(--md-sys-typescale-label-small-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-label-small-line-height);
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
		line-height: var(--md-sys-typescale-body-medium-line-height);
	}
	.md-content :global(li) {
		margin-bottom: 0.25em;
		line-height: inherit;
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
		cursor: grab;
		touch-action: pan-x;
		width: 100%;
		margin: 0 0 0.75em;
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		scrollbar-width: thin;
		scrollbar-color: var(--md-sys-color-outline-variant) transparent;
	}
	.md-content :global(th),
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
	.md-content :global(pre.drag-scroll--active),
	.md-content :global(table.drag-scroll--active) {
		cursor: grabbing;
		user-select: none;
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
	/* Streaming: hide edge fades entirely — MutationObserver-driven hint
	 * refreshes would otherwise flicker at the scroll edges every chunk. */
	.md-content.streaming :global(.md-code-wrap)::before,
	.md-content.streaming :global(.md-code-wrap)::after,
	.md-content.streaming :global(.md-table-wrap)::before,
	.md-content.streaming :global(.md-table-wrap)::after {
		content: none;
		opacity: 0;
		transition: none;
	}
	.md-content :global(.md-code-wrap)::before,
	.md-content :global(.md-table-wrap)::before {
		left: 0;
		opacity: var(--sh-l, 0);
		background: linear-gradient(
			to right,
			var(--md-sys-color-surface-container-high),
			transparent
		);
		border-radius: var(--md-sys-shape-small) 0 var(--md-sys-shape-small) 0;
	}
	.md-content :global(.md-code-wrap)::after,
	.md-content :global(.md-table-wrap)::after {
		right: 0;
		opacity: var(--sh-r, 0);
		background: linear-gradient(
			to left,
			var(--md-sys-color-surface-container-high),
			transparent
		);
		border-radius: 0 var(--md-sys-shape-small) 0 var(--md-sys-shape-small);
	}
	.md-content :global(.md-table-wrap)::before {
		background: linear-gradient(
			to right,
			color-mix(
				in srgb,
				var(--md-sys-color-primary-container) 20%,
				var(--md-sys-color-surface)
			),
			transparent
		);
		border-radius: 0;
	}
	.md-content :global(.md-table-wrap)::after {
		background: linear-gradient(
			to left,
			color-mix(
				in srgb,
				var(--md-sys-color-primary-container) 20%,
				var(--md-sys-color-surface)
			),
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
		margin: 0 0 0.6em;
		color: var(--md-sys-color-on-surface);
		line-height: var(--md-sys-typescale-title-medium-line-height);
		overflow-wrap: anywhere;
	}
	.md-content :global(h1) {
		font-size: var(--md-sys-typescale-title-large-size);
	}
	.md-content :global(h2) {
		font-size: var(--md-sys-typescale-title-medium-size);
	}
	.md-content :global(h3) {
		font-size: var(--md-sys-typescale-body-large-size);
	}
	.md-content :global(h4) {
		font-size: var(--md-sys-typescale-body-medium-size);
	}
	.reasoning-block {
		background: color-mix(
			in srgb,
			var(--md-sys-color-primary-container) 24%,
			var(--md-sys-color-surface-container-low)
		);
		border: 1px dashed
			color-mix(in srgb, var(--md-sys-color-primary) 35%, var(--md-sys-color-outline-variant));
		border-left: 3px solid color-mix(in srgb, var(--md-sys-color-primary) 62%, transparent);
		border-radius: var(--md-sys-shape-small);
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		font-size: var(--md-sys-typescale-body-small-size);
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.reasoning-summary {
		color: var(--md-sys-color-primary);
		font-weight: 600;
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.reasoning-content {
		margin-top: var(--md-sys-space-xs);
		color: var(--md-sys-color-on-surface-variant);
	}
	@media (max-width: 640px) {
		.bubble {
			padding-inline: var(--md-sys-space-md);
		}
		.bubble.user {
			width: 94%;
			max-width: 94%;
		}
		.bubble.tool {
			width: 100%;
			max-width: 100%;
		}
		.bubble.thinking {
			padding-inline: var(--md-sys-space-xs);
		}
	}
</style>
