import { addNotification } from '$lib/stores.ts';

/**
 * Copy `text` to the clipboard and surface a toast. Shared by every context
 * menu so the empty-value guard and notification copy stay consistent.
 * @param {string} text
 * @param {string} label Suffix for the success toast, e.g. '名称' -> '已复制名称'
 * @returns {Promise<boolean>} true when the clipboard write succeeded
 */
export async function copyText(text, label = '') {
	const value = typeof text === 'string' ? text.trim() : '';
	if (!value) {
		addNotification('复制失败：内容为空', 'error', 2000);
		return false;
	}
	try {
		await navigator.clipboard.writeText(value);
		addNotification(`已复制${label}`, 'info', 1500);
		return true;
	} catch {
		addNotification('复制失败', 'error', 2000);
		return false;
	}
}
