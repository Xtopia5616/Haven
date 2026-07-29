<script>
	import { onMount } from 'svelte';

	let { role, content, type: msgType, time, voice = false, streaming = false, toolName = '', messageId = '', stepNumber = null, onContextMenu = null } = $props();

	let md = $state(null);
	let mdHtml = $state('');

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
				catch (e) { console.warn('[ChatBubble] highlight failed:', e); return ''; }
			},
		});
	});

	$effect(() => {
		if (!md || role !== 'assistant' || msgType) return;
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
>
	<div class="bubble-header">
		<span class="bubble-role">
			{role === 'user' ? 'You' : 'Haven'}
			{#if voice}<span class="mic-icon" title="Voice input">&#127908;</span>{/if}
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
			<details class="reasoning-block" open={streaming}>
				<summary class="reasoning-summary">Thinking...</summary>
				<div class="reasoning-content"><em>{content}{#if streaming && content}<span class="caret"></span>{/if}</em></div>
			</details>
		{:else if msgType === 'tool'}
			<div class="tool-call">&#9654; Calling {toolName}</div>
			{#if content}
				<details class="observation-block" open={streaming}>
					<summary class="observation-summary">Result</summary>
					<pre class="observation">{content}</pre>
				</details>
			{/if}
		{:else if msgType === 'supplement'}
			<div class="supplement-badge">&#10100; {content}</div>
		{:else if role === 'assistant'}
			{#if mdHtml}
				<div class="md-content" class:streaming>{@html mdHtml}{#if streaming && content}<span class="caret"></span>{/if}</div>
			{:else}
				<p>{content}{#if streaming && content}<span class="caret"></span>{/if}</p>
			{/if}
		{:else}
			<p>{content}</p>
		{/if}
	</div>
</div>

<style>
	.bubble {
		max-width: 85%;
		min-width: 30%;
		width: fit-content;
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border-radius: var(--md-sys-shape-large);
		font-size: 13px;
		line-height: 1.5;
		animation: bubbleIn var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-emphasized);
	}
	@keyframes bubbleIn {
		from {
			opacity: 0;
			transform: translateY(4px);
		}
		to {
			opacity: 1;
			transform: translateY(0);
		}
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
