import logger from '$lib/logger.ts';
import { EXT_REF_CLASS, EXT_REF_TITLE } from '$lib/externalRef.ts';
import { pathifyPlugin } from '$lib/pathify.ts';
import type MarkdownIt from 'markdown-it';

// Module-level lazy singleton for the MarkdownIt renderer. Chat bubbles used
// to each dynamically import markdown-it + highlight.js and construct their
// own MarkdownIt instance (8 language registrations + a custom fence rule)
// in onMount — that cost multiplied per bubble and per message. Now the
// instance is built exactly once on first use and shared by every bubble.
let md: MarkdownIt | null = null;
let loading: Promise<MarkdownIt> | null = null;

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
export function getMarkdownRenderer(): Promise<MarkdownIt> {
	if (md) return Promise.resolve(md);
	if (loading) return loading;
	const build = (async () => {
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
		// Wrap every table in a non-scrolling container. The edge fade hints
		// live on this wrapper (positioned overlays), so they stay fixed at the
		// viewport edges while the table scrolls inside it. Fades placed
		// directly on the scroll container would scroll along with the content.
		instance.renderer.rules.table_open = () =>
			'<div class="md-table-wrap"><table>';
		instance.renderer.rules.table_close = () => '</table></div>';

		// Bare filesystem paths → `.ext-ref` anchors (same interaction as URLs).
		pathifyPlugin(instance);

		// All links (markdown, linkify, pathify) share copy / Ctrl+open behavior.
		const defaultLinkOpen =
			instance.renderer.rules.link_open ||
			((tokens, idx, options, env, self) => self.renderToken(tokens, idx, options));
		instance.renderer.rules.link_open = (tokens, idx, options, env, self) => {
			const token = tokens[idx];
			const href = token.attrGet('href') || '';
			const isPath = token.attrGet('data-ext') === 'path';
			if (!token.attrGet('data-target')) {
				token.attrSet('data-target', href);
			}
			// Neutralize the live href so middle-click / auxclick / other
			// browser activations cannot invoke OS URI handlers (ms-msdt:,
			// search-ms:, …) and bypass open_external allowlisting. Copy /
			// Ctrl+open read data-target instead.
			token.attrSet('href', '#');
			token.attrJoin('class', isPath ? `${EXT_REF_CLASS} ext-ref-path` : `${EXT_REF_CLASS} ext-ref-url`);
			token.attrSet('title', EXT_REF_TITLE);
			if (!isPath) {
				token.attrSet('rel', 'noopener noreferrer');
			}
			return defaultLinkOpen(tokens, idx, options, env, self);
		};

		md = instance;
		return instance;
	})();
	loading = build;
	return build;
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
export function renderMarkdown(text: string, isStreaming = false) {
	if (!md) return '';
	streaming = isStreaming;
	try {
		// `havenStreaming` gates pathify (and keeps fence deferral in sync) so
		// live previews skip path scanning; the final render linkifies paths.
		return md.render(text, { havenStreaming: isStreaming });
	} finally {
		streaming = false;
	}
}
