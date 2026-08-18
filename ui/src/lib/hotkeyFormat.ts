/**
 * Map a KeyboardEvent to the backend `parse_shortcut` format string
 * (e.g. "Ctrl+Shift+Space"), or null if the combo is not yet complete (only
 * modifiers held) or unsupported.
 *
 * This mirrors the key set accepted by `parse_shortcut`
 * (crates/app-binary/src/lib.rs): modifiers Ctrl/Shift/Alt/Super + a-z,
 * Space, Enter, Tab, F1-F12. It intentionally produces a *subset* of what
 * `parse_shortcut` accepts — bare letters and Escape are parseable by the
 * backend but deliberately not emitted here (a bare letter would conflict
 * with typing; Escape is the capture-cancel gesture). Keeping this logic in a
 * plain module (not inline in the component) lets it be unit-tested for
 * parity with the backend.
 *
 * @param {KeyboardEvent} e
 * @returns {string | null}
 */
export function formatCombo(e) {
	const mods = [];
	if (e.ctrlKey) mods.push('Ctrl');
	if (e.shiftKey) mods.push('Shift');
	if (e.altKey) mods.push('Alt');
	if (e.metaKey) mods.push('Super');

	// Translate the physical key via `code` (layout-independent) so the
	// binding matches what the OS-level global shortcut registers.
	const code = e.code || '';
	let key = null;
	if (code.startsWith('Key') && code.length === 4) {
		key = code.slice(3).toUpperCase();
	} else if (code.startsWith('Digit') && code.length === 6) {
		// parse_shortcut does not support digits; reject.
		key = null;
	} else if (/^F([1-9]|1[0-2])$/.test(code)) {
		key = code;
	} else {
		switch (code) {
			case 'Space': key = 'Space'; break;
			case 'Enter': key = 'Enter'; break;
			case 'Tab': key = 'Tab'; break;
			default: key = null;
		}
	}
	if (!key) return null;
	// Require at least one modifier unless it's a function key (F-keys are
	// commonly used standalone as global hotkeys).
	if (mods.length === 0 && !/^F\d+$/.test(key)) return null;
	return [...mods, key].join('+');
}
