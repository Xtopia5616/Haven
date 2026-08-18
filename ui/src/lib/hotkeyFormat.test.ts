import { describe, it, expect } from 'vitest';
import { formatCombo } from './hotkeyFormat.ts';

/**
 * Parity contract with the backend `parse_shortcut`
 * (crates/app-binary/src/lib.rs). Every combo emitted here MUST be parseable
 * by the backend: modifier names Ctrl/Shift/Alt/Super (note: the backend also
 * accepts Control/Win/Cmd aliases, but the UI only emits the canonical names),
 * keys A-Z / Space / Enter / Tab / F1-F12, joined with '+', modifiers first in
 * a fixed order, then the key. Function keys may stand alone.
 */
function ev(props) {
	return new KeyboardEvent('keydown', props);
}

describe('formatCombo parity with parse_shortcut', () => {
	it('emits modifiers in the fixed order Ctrl, Shift, Alt, Super', () => {
		const combo = formatCombo(
			ev({ code: 'KeyA', ctrlKey: true, shiftKey: true, altKey: true, metaKey: true }),
		);
		expect(combo).toBe('Ctrl+Shift+Alt+Super+A');
	});

	it('emits a single modifier + letter', () => {
		expect(formatCombo(ev({ code: 'KeyK', ctrlKey: true }))).toBe('Ctrl+K');
		expect(formatCombo(ev({ code: 'KeyD', metaKey: true }))).toBe('Super+D');
	});

	it('emits function keys, optionally standalone', () => {
		expect(formatCombo(ev({ code: 'F5' }))).toBe('F5');
		expect(formatCombo(ev({ code: 'F12', ctrlKey: true }))).toBe('Ctrl+F12');
	});

	it('emits Space/Enter/Tab with a modifier', () => {
		expect(formatCombo(ev({ code: 'Space', ctrlKey: true, shiftKey: true }))).toBe(
			'Ctrl+Shift+Space',
		);
		expect(formatCombo(ev({ code: 'Enter', altKey: true }))).toBe('Alt+Enter');
		expect(formatCombo(ev({ code: 'Tab', ctrlKey: true }))).toBe('Ctrl+Tab');
	});

	it('rejects bare letters (no modifier)', () => {
		expect(formatCombo(ev({ code: 'KeyA' }))).toBeNull();
		expect(formatCombo(ev({ code: 'KeyZ' }))).toBeNull();
	});

	it('rejects digit keys (parse_shortcut does not support digits)', () => {
		expect(formatCombo(ev({ code: 'Digit1', ctrlKey: true }))).toBeNull();
		expect(formatCombo(ev({ code: 'Digit0' }))).toBeNull();
	});

	it('rejects unsupported codes', () => {
		expect(formatCombo(ev({ code: 'ArrowLeft', ctrlKey: true }))).toBeNull();
		expect(formatCombo(ev({ code: '' }))).toBeNull();
	});

	it('rejects function keys outside F1-F12', () => {
		expect(formatCombo(ev({ code: 'F13' }))).toBeNull();
		expect(formatCombo(ev({ code: 'F0' }))).toBeNull();
	});

	it('rejects a modifiers-only combo', () => {
		expect(formatCombo(ev({ ctrlKey: true, shiftKey: true }))).toBeNull();
	});
});
