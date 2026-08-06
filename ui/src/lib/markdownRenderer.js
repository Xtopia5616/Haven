import logger from '$lib/logger.js';

// Module-level lazy singleton for the MarkdownIt renderer. Chat bubbles used
// to each dynamically import markdown-it + highlight.js and construct their
// own MarkdownIt instance (8 language registrations + a custom fence rule)
// in onMount — that cost multiplied per bubble and per message. Now the
// instance is built exactly once on first use and shared by every bubble.
let md = null;
let loading = null;

// Streaming flag read by the fence rule. While true, code blocks render as a
// plain <pre> (no highlight, no language bar / copy button): half-typed
// fences never flicker and per-chunk cost stays low. The full highlighted
// block is produced once the stream ends.
let streaming = false;

/**
 * Resolve the shared MarkdownIt instance. Returns the same promise for
 * concurrent callers so the heavy import/registration work happens exactly
 * once.
 * @returns {Promise<import('markdown-it').default>}
 */
export function getMarkdownRenderer() {
	if (md) return Promise.resolve(md);
	if (loading) return loading;
	loading = (async () => {
		const [{ default: MarkdownIt }, hljs, javascript, typescript, bash, json, css, xml, rust, yaml] =
			await Promise.all([
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
		const instance = new MarkdownIt({
			html: false,
			linkify: true,
			breaks: true,
			highlight(str, lang) {
				if (!lang || !highlighter.getLanguage(lang)) return '';
				try { return highlighter.highlight(str, { language: lang }).value; }
				catch (e) { logger.warn('markdownRenderer', 'highlight failed', e); return ''; }
			},
		});
		// Wrap every code fence in the same container style as the JsonView
		// tool cards: a toolbar with language label + copy button above the
		// highlighted code. Copy clicks are delegated on the container.
		instance.renderer.rules.fence = (tokens, idx) => {
			const token = tokens[idx];
			const esc = instance.utils.escapeHtml;
			if (streaming) {
				return `<pre class="md-code-streaming"><code>${esc(token.content)}</code></pre>`;
			}
			const info = token.info ? instance.utils.unescapeAll(token.info).trim() : '';
			const lang = info.split(/\s+/g)[0];
			let code;
			if (lang && highlighter.getLanguage(lang)) {
				try { code = highlighter.highlight(token.content, { language: lang }).value; }
				catch (e) { logger.warn('markdownRenderer', 'highlight failed', e); code = esc(token.content); }
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
		md = instance;
		return md;
	})();
	return loading;
}

/**
 * Render markdown synchronously with the shared instance. Call only after
 * `getMarkdownRenderer()` has resolved. When `streaming` is true, code
 * fences are deferred: they render as a plain <pre> without highlight or
 * copy bar, and the full highlighted block is produced once streaming ends.
 * @param {string} text
 * @param {boolean} [isStreaming]
 * @returns {string}
 */
export function renderMarkdown(text, isStreaming = false) {
	if (!md) return '';
	streaming = isStreaming;
	try {
		return md.render(text);
	} finally {
		streaming = false;
	}
}
