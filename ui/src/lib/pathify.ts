/**
 * Detect absolute filesystem paths inside markdown-it text tokens so they can
 * be rendered as `.ext-ref` links (copy / Ctrl+open).
 *
 * Patterns (absolute only — relative/UNC are never auto-linked, because
 * `open_external` rejects them for safety and CWD correctness):
 * - Windows drive: `D:\foo\bar` / `D:/foo/bar`
 * - Unix absolute: `/home/user/file`
 */

import type MarkdownIt from 'markdown-it';
import type Token from 'markdown-it/lib/token.mjs';

/** markdown-it nesting: 1 = open, 0 = self-closing, -1 = close. */
type Nesting = 1 | 0 | -1;
type TokenCtor = new (type: string, tag: string, nesting: Nesting) => Token;

/**
 * Global matcher. Groups:
 * 1 = Windows drive path
 * 2 = Unix absolute (may be preceded by a delimiter outside the group)
 */
export const PATH_FIND_RE = new RegExp(
	[
		// Windows absolute (drive letter)
		String.raw`([A-Za-z]:(?:[\\/][^\s<>"|?*\`'()\[\]{}]+)+)`,
		// Unix absolute at a boundary; skip protocol-like // and UNC
		String.raw`(?:^|[\s(\`"'])(/(?!/)[^\s<>"|?*\`'()\[\]{}]+)`,
	].join('|'),
	'g',
);

/** Cheap prefilter: absolute paths always contain `:` (Windows) or `/` (Unix). */
const PATH_HINT_RE = /[/\\:]/;

/** Trailing punctuation that often sticks to paths in prose. */
const TRAIL_PUNCT_RE = /[.,;:!?)\]\}。，；：！？）】」』]+$/;

/** Trim trailing punctuation from a matched path. */
export function trimPathMatch(path: string): string {
	return path.replace(TRAIL_PUNCT_RE, '');
}

/**
 * Walk an inline token's children and wrap path matches in link tokens.
 * Skips content already inside links. Returns the original array when nothing
 * changed so streaming renders avoid needless reallocations.
 */
export function pathifyChildren(children: Token[], Token: TokenCtor): Token[] {
	const out: Token[] = [];
	let linkDepth = 0;
	let changed = false;

	for (const child of children) {
		if (child.type === 'link_open') {
			linkDepth += 1;
			out.push(child);
			continue;
		}
		if (child.type === 'link_close') {
			linkDepth = Math.max(0, linkDepth - 1);
			out.push(child);
			continue;
		}
		if (linkDepth > 0 || child.type !== 'text' || !child.content) {
			out.push(child);
			continue;
		}

		const pieces = splitTextByPaths(child, Token);
		if (pieces.length === 1 && pieces[0] === child) {
			out.push(child);
		} else {
			changed = true;
			out.push(...pieces);
		}
	}
	return changed ? out : children;
}

function splitTextByPaths(child: Token, Token: TokenCtor): Token[] {
	const text = child.content;
	if (!PATH_HINT_RE.test(text)) {
		return [child];
	}

	PATH_FIND_RE.lastIndex = 0;
	const tokens: Token[] = [];
	let last = 0;
	let found = false;
	let match: RegExpExecArray | null;
	while ((match = PATH_FIND_RE.exec(text)) !== null) {
		const pathRaw = match[1] || match[2] || '';
		const path = trimPathMatch(pathRaw);
		if (!path || path.length < 2) continue;

		const pathStartInMatch = match[0].lastIndexOf(pathRaw);
		const absStart = match.index + (pathStartInMatch >= 0 ? pathStartInMatch : 0);
		const absEnd = absStart + path.length;
		if (absStart < last) continue;

		if (absStart > last) {
			tokens.push(makeTextToken(Token, text.slice(last, absStart)));
		}
		tokens.push(...makePathLinkTokens(Token, path));
		last = absEnd;
		found = true;
		PATH_FIND_RE.lastIndex = absEnd;
	}
	if (!found) {
		return [child];
	}
	if (last < text.length) {
		tokens.push(makeTextToken(Token, text.slice(last)));
	}
	return tokens;
}

function makeTextToken(Token: TokenCtor, content: string): Token {
	const tok = new Token('text', '', 0);
	tok.content = content;
	return tok;
}

function makePathLinkTokens(Token: TokenCtor, path: string): Token[] {
	const open = new Token('link_open', 'a', 1);
	open.attrs = [
		['href', path],
		['data-target', path],
		['data-ext', 'path'],
	];
	const text = new Token('text', '', 0);
	text.content = path;
	const close = new Token('link_close', 'a', -1);
	return [open, text, close];
}

/**
 * markdown-it plugin: after `text_join` (so backslash-escaped fragments are
 * already merged into plain `text` tokens), turn bare absolute paths into links.
 * Skipped while streaming (same deferral pattern as code fences).
 */
export function pathifyPlugin(md: MarkdownIt): void {
	md.core.ruler.after('text_join', 'haven_pathify', (state) => {
		if (state.env?.havenStreaming) return;
		const Token = state.Token;
		for (const block of state.tokens) {
			if (block.type !== 'inline' || !block.children?.length) continue;
			block.children = pathifyChildren(block.children, Token);
		}
	});
}
