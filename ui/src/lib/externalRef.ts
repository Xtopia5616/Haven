import { copyText } from '$lib/clipboard.ts';
import { invoke } from '$lib/tauri.ts';
import logger from '$lib/logger.ts';
import { addNotification } from '$lib/stores.ts';

/** Native tooltip shown on every external ref (URL or filesystem path). */
export const EXT_REF_TITLE = '单击复制 · 按住 Ctrl 再点击打开';

/** CSS class applied to interactive URL/path anchors. */
export const EXT_REF_CLASS = 'ext-ref';

/**
 * Resolve the target string from an ext-ref element (data-target, then href,
 * then text content).
 * @param {Element | null | undefined} el
 * @returns {string}
 */
export function extRefTarget(el) {
	if (!el) return '';
	const data = el.getAttribute?.('data-target');
	if (data && data.trim()) return data.trim();
	const href = el.getAttribute?.('href');
	if (href && href !== '#' && !href.startsWith('javascript:')) return href.trim();
	return (el.textContent || '').trim();
}

/**
 * Open a URL in the default browser, or a filesystem path in the file manager.
 * @param {string} target
 * @returns {Promise<boolean>}
 */
export async function openExternal(target) {
	const value = typeof target === 'string' ? target.trim() : '';
	if (!value) {
		addNotification('打开失败：目标为空', 'error', 2000);
		return false;
	}
	try {
		await invoke('open_external', { target: value });
		return true;
	} catch (e) {
		logger.warn('externalRef', 'open_external failed', e);
		addNotification('打开失败', 'error', 2000);
		return false;
	}
}

/**
 * Click / contextmenu handler for `.ext-ref` elements.
 * - Left or right click → copy
 * - Ctrl/Meta + click → open in browser / file manager
 * Returns true when the event targeted an ext-ref (caller should stop bubble menus).
 * @param {MouseEvent} e
 * @returns {boolean}
 */
export function handleExtRefEvent(e) {
	const el = /** @type {Element | null} */ (
		e.target instanceof Element ? e.target.closest(`.${EXT_REF_CLASS}`) : null
	);
	if (!el) return false;

	e.preventDefault();
	e.stopPropagation();

	const target = extRefTarget(el);
	if (!target) return true;

	const open = e.ctrlKey || e.metaKey;
	if (open && e.type === 'click') {
		void openExternal(target);
		return true;
	}

	// Right-click and plain left-click both copy. Ctrl+contextmenu still copies
	// (open is reserved for left-click with modifier).
	const label = looksLikeUrl(target) ? '链接' : '路径';
	void copyText(target, label);
	return true;
}

/**
 * @param {string} value
 * @returns {boolean}
 */
export function looksLikeUrl(value) {
	const lower = value.trim().toLowerCase();
	return lower.startsWith('http://') || lower.startsWith('https://');
}
