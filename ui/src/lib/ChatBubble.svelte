<script>
	import { onDestroy, onMount, untrack } from 'svelte';
	import { fly } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';
	import logger from '$lib/logger.js';
	import { imageDataUrl } from '$lib/stores.js';
	import ToolResultCard, { canRenderToolResult } from '$lib/ToolResultCard.svelte';

	let { role, content, type: msgType, time, voice = false, streaming = false, toolName = '', messageId = '', stepNumber = null, attachments = [], options = [], awaiting = false, received = false, onContextMenu = null, onQuickReply = null } = $props();

	// Local open state for collapsible <details> blocks (reasoning +
	// tool observations). The block expands while streaming so live output is
	// visible, and auto-collapses once streaming ends (constraint
	// tool_call_output_expand_during_collapse_after). Manual clicks after that
	// persist: binding `open={streaming}` directly would re-apply the value on
	// every content-driven re-render, overriding a manual toggle.
	//
	// $effect.pre runs before the DOM updates, so the open/collapse happens
	// in the same frame as the streaming transition — no flash where the
	// block briefly renders open before snapping shut.
	let reasoningOpen = $state(untrack(() => streaming));
	let observationOpen = $state(untrack(() => streaming));
	let lastStreaming = untrack(() => streaming);
	$effect.pre(() => {
		// Only react to streaming TRANSITIONS, not to every re-render, so a
		// manual toggle is never clobbered.
		if (streaming === lastStreaming) return;
		if (streaming) {
			// Streaming (re)started → expand so live output is visible.
			reasoningOpen = true;
			observationOpen = true;
		} else {
			// Streaming ended → auto-collapse once.
			reasoningOpen = false;
			observationOpen = false;
		}
		lastStreaming = streaming;
	});

	let md = $state(null);
	let mdHtml = $state('');
	// L11: the component may be destroyed while onMount's dynamic imports are
	// still resolving; guard state writes against an unmounted component.
	let mounted = true;

	onDestroy(() => {
		mounted = false;
	});

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
			onContextMenu({ x: e.clientX, y: e.clientY, messageId, stepNumber, role, content, type: msgType, selectedContent });
		}
	}

	// Copy buttons inside rendered markdown code fences (md-code-copy). Click
	// delegation survives re-renders of {@html} content and works during
	// streaming. The whole text of the code block is copied, matching what is
	// highlighted, without any trailing newline.
	function handleMdContentClick(e) {
		const btn = e.target.closest?.('.md-code-copy');
		if (!btn) return;
		const wrap = btn.closest('.md-code-wrap');
		const codeEl = wrap?.querySelector('code');
		if (!codeEl) return;
		e.preventDefault();
		e.stopPropagation();
		const text = codeEl.textContent ?? '';
		navigator.clipboard?.writeText(text)
			.then(() => {
				const label = btn.querySelector('.md-code-copy-text');
				if (!label) return;
				const original = label.textContent;
				label.textContent = '已复制';
				setTimeout(() => { label.textContent = original; }, 1500);
			})
			.catch(() => {});
	}

	// `use:mdContentClick` attaches the delegation listener to the rendered
	// markdown container. Wrapping it in an action (instead of `onclick` on the
	// div) keeps the div non-interactive for a11y: only the real copy buttons
	// inside are clickable.
	function mdContentClick(node) {
		node.addEventListener('click', handleMdContentClick);
		return { destroy() { node.removeEventListener('click', handleMdContentClick); } };
	}

	onMount(async () => {
		const [MarkdownIt, hljs, javascript, typescript, bash, json, css, xml, rust, yaml] = await Promise.all([
			import('markdown-it'),
			import('highlight.js/lib/core'),
			import('highlight.js/lib/languages/javascript'),
			import('highlight.js/lib/languages/typescript'),
			import('highlight.js/lib/languages/bash'),
			import('highlight.js/lib/languages/json'),
			import('highlight.js/lib/languages/css'),
			import('highlight.js/lib/languages/xml'),
			import('highlight.js/lib/languages/rust'),
			import('highlight.js/lib/languages/yaml'),
		]);
		if (!mounted) return;
		const highlighter = hljs.default;
		highlighter.registerLanguage('javascript', javascript.default);
		highlighter.registerLanguage('typescript', typescript.default);
		highlighter.registerLanguage('bash', bash.default);
		highlighter.registerLanguage('json', json.default);
		highlighter.registerLanguage('css', css.default);
		highlighter.registerLanguage('xml', xml.default);
		highlighter.registerLanguage('rust', rust.default);
		highlighter.registerLanguage('yaml', yaml.default);
		md = new MarkdownIt.default({
			html: false,
			linkify: true,
			breaks: true,
			highlight(str, lang) {
				if (!lang || !highlighter.getLanguage(lang)) return '';
				try { return highlighter.highlight(str, { language: lang }).value; }
				catch (e) { logger.warn('ChatBubble', 'highlight failed', e); return ''; }
			},
		});
		// Wrap every code fence in the same container style as the JsonView
		// tool cards: a toolbar with language label + copy button above the
		// highlighted code. Copy clicks are delegated on the container.
		md.renderer.rules.fence = (tokens, idx) => {
			const token = tokens[idx];
			const info = token.info ? md.utils.unescapeAll(token.info).trim() : '';
			const lang = info.split(/\s+/g)[0];
			const esc = md.utils.escapeHtml;
			let code;
			if (lang && highlighter.getLanguage(lang)) {
				try { code = highlighter.highlight(token.content, { language: lang }).value; }
				catch (e) { logger.warn('ChatBubble', 'highlight failed', e); code = esc(token.content); }
			} else {
				code = esc(token.content);
			}
			return `<div class="md-code-wrap">
				<div class="md-code-bar">
					<span class="md-code-lang">${lang ? esc(lang) : 'text'}</span>
					<button type="button" class="md-code-copy" aria-label="复制代码">
						<svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2" /><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" /></svg>
						<span class="md-code-copy-text">复制</span>
					</button>
				</div>
				<pre><code class="hljs">${code}</code></pre>
			</div>`;
		};
	});

	// L11: guard the render effect against unmount mid-import. mdHtml stays ''
	// until both `md` is loaded and this bubble is still mounted.
	$effect(() => {
		if (!mounted || !md || role !== 'assistant' || msgType) return;
		const text = content || '';
		mdHtml = text ? md.render(text) : '';
	});
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
			{#if role === 'user' && received}<span class="received-tag" title="Agent 已收到">✓</span>{/if}
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
				<div class="reasoning-content"><em>{content}{#if streaming && content}<span class="caret"></span>{/if}</em></div>
			</details>
		{:else if msgType === 'tool'}
			<div class="tool-call">&#9654; Calling {toolName}</div>
			{#if content}
				{#if canRenderToolResult(toolName, content)}
					<ToolResultCard {toolName} {content} />
				{:else}
					<details class="observation-block" bind:open={observationOpen}>
						<summary class="observation-summary">Result</summary>
						<pre class="observation">{content}</pre>
					</details>
				{/if}
			{/if}
		{:else if msgType === 'ask'}
			<ToolResultCard
				type="ask"
				content={content}
				options={options}
				awaiting={awaiting}
				messageId={messageId}
				onQuickReply={onQuickReply}
			/>
		{:else if msgType === 'supplement'}
			<div class="supplement-badge">&#10100; {content}</div>
		{:else if role === 'assistant'}
			{#if mdHtml}
				<div class="md-content" class:streaming use:mdContentClick>{@html mdHtml}{#if streaming && content}<span class="caret"></span>{/if}</div>
			{:else}
				<p>{content}{#if streaming && content}<span class="caret"></span>{/if}</p>
			{/if}
		{:else}
			{#if attachments && attachments.length > 0}
				<div class="attachments">
					{#each attachments as att}
						<img class="attachment-img" src={imageDataUrl(att)} alt="用户发送的图片" loading="lazy" />
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
		background: color-mix(in srgb, var(--md-sys-color-primary) 78%, var(--md-sys-color-surface));
		color: var(--md-sys-color-on-primary);
		border: none;
		border-radius: var(--md-sys-shape-large) var(--md-sys-shape-large) var(--md-sys-shape-extra-small) var(--md-sys-shape-large);
	}
	.bubble.assistant {
		margin-right: auto;
		background: color-mix(in srgb, var(--md-sys-color-primary-container) 20%, var(--md-sys-color-surface));
		color: var(--md-sys-color-on-primary-container);
		border: none;
		border-radius: var(--md-sys-shape-large) var(--md-sys-shape-large) var(--md-sys-shape-large) var(--md-sys-shape-extra-small);
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
	.observation-block {
		margin-top: var(--md-sys-space-xs);
	}
	.observation-summary {
		color: var(--md-sys-color-on-surface-variant);
		font-weight: 600;
		cursor: pointer;
		font-size: 11px;
		user-select: none;
		padding: 2px 0;
	}
	.observation-block[open] .observation-summary {
		margin-bottom: var(--md-sys-space-xs);
	}
	.observation {
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
		padding: var(--md-sys-space-md);
		border-radius: var(--md-sys-shape-small);
		font-family: var(--md-sys-typescale-mono);
		font-size: 11px;
		white-space: pre-wrap;
		overflow-x: auto;
	}
	.md-content :global(p) { margin: 0 0 0.75em; }
	.md-content :global(p:last-child) { margin-bottom: 0; }
	.md-content :global(pre) {
		background: var(--md-sys-color-surface-container-high);
		padding: var(--md-sys-space-md);
		border-radius: var(--md-sys-shape-small);
		font-size: 12px;
		overflow-x: auto;
		margin: 0 0 0.75em;
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
	.md-content :global(.md-code-wrap) {
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
		transition: background-color 0.15s ease, color 0.15s ease, border-color 0.15s ease;
	}
	.md-content :global(.md-code-copy:hover) {
		background: var(--md-sys-color-surface-container-highest);
		color: var(--md-sys-color-on-surface);
	}
	.md-content :global(.md-code-copy svg) {
		flex: none;
	}
	.md-content :global(.hljs-keyword) { color: var(--md-sys-color-primary); }
	.md-content :global(.hljs-string) { color: var(--md-sys-color-success); }
	.md-content :global(.hljs-number) { color: var(--md-sys-color-tertiary); }
	.md-content :global(.hljs-comment) { color: var(--md-sys-color-on-surface-variant); font-style: italic; opacity: 0.7; }
	.md-content :global(.hljs-function) { color: var(--md-sys-color-primary); }
	.md-content :global(.hljs-title) { color: var(--md-sys-color-primary); }
	.md-content :global(.hljs-params) { color: var(--md-sys-color-on-surface); }
	.md-content :global(.hljs-built_in) { color: var(--md-sys-color-tertiary); }
	.md-content :global(.hljs-type) { color: color-mix(in srgb, var(--md-sys-color-tertiary) 80%, var(--md-sys-color-primary)); }
	.md-content :global(.hljs-literal) { color: var(--md-sys-color-primary); }
	.md-content :global(.hljs-selector-class) { color: var(--md-sys-color-tertiary); }
	.md-content :global(.hljs-title.class_) { color: var(--md-sys-color-tertiary); }
	.md-content :global(.hljs-selector-tag) { color: var(--md-sys-color-primary); }
	.md-content :global(.hljs-attr) { color: var(--md-sys-color-tertiary); }
	.md-content :global(.hljs-attribute) { color: var(--md-sys-color-tertiary); }
	.md-content :global(.hljs-variable) { color: var(--md-sys-color-error); }
	.md-content :global(.hljs-meta) { color: var(--md-sys-color-on-surface-variant); opacity: 0.7; }
	.md-content :global(.hljs-property) { color: var(--md-sys-color-on-surface); }
	.md-content :global(.hljs-punctuation) { color: var(--md-sys-color-on-surface-variant); }
	.md-content :global(.hljs-operator) { color: var(--md-sys-color-on-surface-variant); }
	.md-content :global(ul),
	.md-content :global(ol) {
		padding-left: 1.5em;
		margin: 0 0 0.75em;
	}
	.md-content :global(li) { margin-bottom: 0.25em; }
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
		border-collapse: collapse;
		display: block;
		overflow-x: auto;
		width: 100%;
		margin: 0 0 0.75em;
		font-size: 12px;
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
	.md-content :global(strong) { font-weight: 700; }
	.md-content :global(a) {
		color: var(--md-sys-color-primary);
		text-decoration: underline;
	}
	.md-content :global(h1),
	.md-content :global(h2),
	.md-content :global(h3),
	.md-content :global(h4) {
		font-weight: 600;
		margin: 0 0 0.5em;
		color: var(--md-sys-color-on-surface);
	}
	.md-content :global(h1) { font-size: 17px; }
	.md-content :global(h2) { font-size: 15px; }
	.md-content :global(h3) { font-size: 14px; }
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
