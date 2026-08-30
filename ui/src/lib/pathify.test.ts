import { describe, expect, it } from 'vitest';
import { PATH_FIND_RE, pathifyChildren, trimPathMatch } from './pathify.ts';
import { getMarkdownRenderer, renderMarkdown } from './markdownRenderer.ts';
import MarkdownIt from 'markdown-it';

describe('trimPathMatch', () => {
	it('strips trailing punctuation', () => {
		expect(trimPathMatch('D:\\a\\b.rs。')).toBe('D:\\a\\b.rs');
		expect(trimPathMatch('/tmp/foo.txt.')).toBe('/tmp/foo.txt');
		expect(trimPathMatch('D:\\a\\b.rs)')).toBe('D:\\a\\b.rs');
	});
});

describe('PATH_FIND_RE', () => {
	it('matches Windows absolute paths', () => {
		PATH_FIND_RE.lastIndex = 0;
		const m = PATH_FIND_RE.exec('see D:\\Workspace\\Haven\\ui\\src\\lib\\x.ts please');
		expect(m?.[1]).toBe('D:\\Workspace\\Haven\\ui\\src\\lib\\x.ts');
	});

	it('matches forward-slash Windows paths', () => {
		PATH_FIND_RE.lastIndex = 0;
		const m = PATH_FIND_RE.exec('open D:/Workspace/Haven/README.md now');
		expect(m?.[1]).toBe('D:/Workspace/Haven/README.md');
	});

	it('does not match relative code paths', () => {
		PATH_FIND_RE.lastIndex = 0;
		const m = PATH_FIND_RE.exec('in crates/llm/src/adapters/openai.rs:42');
		expect(m).toBeNull();
	});

	it('does not match UNC paths', () => {
		PATH_FIND_RE.lastIndex = 0;
		const m = PATH_FIND_RE.exec('see \\\\server\\share\\file.txt here');
		expect(m).toBeNull();
	});

	it('matches unix absolute paths', () => {
		PATH_FIND_RE.lastIndex = 0;
		const m = PATH_FIND_RE.exec('read /home/user/project/file.rs end');
		expect(m?.[2]).toBe('/home/user/project/file.rs');
	});
});

describe('pathifyChildren', () => {
	it('wraps bare paths as link tokens', () => {
		const md = new MarkdownIt({ html: false, linkify: true, breaks: true });
		const state = new md.core.State('see D:\\a\\b.rs here', md, {});
		const text = new state.Token('text', '', 0);
		text.content = 'see D:\\a\\b.rs here';
		const out = pathifyChildren([text], state.Token);
		expect(out.some((t) => t.type === 'link_open')).toBe(true);
		const open = out.find((t) => t.type === 'link_open')!;
		expect(open.attrGet('data-ext')).toBe('path');
		expect(open.attrGet('data-target')).toBe('D:\\a\\b.rs');
		expect(out.find((t) => t.type === 'text' && t.content === 'D:\\a\\b.rs')).toBeTruthy();
	});

	it('returns the original array when nothing matched', () => {
		const md = new MarkdownIt({ html: false, linkify: true, breaks: true });
		const state = new md.core.State('', md, {});
		const text = new state.Token('text', '', 0);
		text.content = 'plain prose without paths';
		const input = [text];
		expect(pathifyChildren(input, state.Token)).toBe(input);
	});

	it('does not nest paths inside existing links', () => {
		const md = new MarkdownIt({ html: false, linkify: true, breaks: true });
		const state = new md.core.State('', md, {});
		const open = new state.Token('link_open', 'a', 1);
		open.attrs = [['href', 'https://example.com']];
		const text = new state.Token('text', '', 0);
		text.content = 'D:\\a\\b.rs';
		const close = new state.Token('link_close', 'a', -1);
		const out = pathifyChildren([open, text, close], state.Token);
		expect(out.filter((t) => t.type === 'link_open')).toHaveLength(1);
		expect(out[1].content).toBe('D:\\a\\b.rs');
	});
});

describe('renderMarkdown pathify integration', () => {
	it('wraps Windows paths after text_join', async () => {
		await getMarkdownRenderer();
		const html = renderMarkdown('open D:\\Workspace\\Haven\\README.md now');
		expect(html).toContain('ext-ref-path');
		expect(html).toContain('D:\\Workspace\\Haven\\README.md');
	});

	it('skips pathify while streaming', async () => {
		await getMarkdownRenderer();
		const html = renderMarkdown('open D:\\Workspace\\Haven\\README.md now', true);
		expect(html).not.toContain('ext-ref-path');
		expect(html).toContain('D:\\Workspace\\Haven\\README.md');
	});

	it('does not wrap relative code paths', async () => {
		await getMarkdownRenderer();
		const html = renderMarkdown('see crates/llm/src/foo.rs here');
		expect(html).not.toContain('ext-ref-path');
	});

	it('wraps autolinked URLs as ext-ref-url', async () => {
		await getMarkdownRenderer();
		const html = renderMarkdown('see https://example.com/docs');
		expect(html).toContain('ext-ref-url');
		expect(html).toContain('data-target="https://example.com/docs"');
	});
});
