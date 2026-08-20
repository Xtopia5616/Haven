import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
	EXT_REF_CLASS,
	extRefTarget,
	handleExtRefEvent,
	looksLikeUrl,
} from './externalRef.ts';

vi.mock('$lib/clipboard.ts', () => ({
	copyText: vi.fn().mockResolvedValue(true),
}));

vi.mock('$lib/tauri.ts', () => ({
	invoke: vi.fn().mockResolvedValue(undefined),
}));

vi.mock('$lib/stores.ts', () => ({
	addNotification: vi.fn(),
}));

vi.mock('$lib/logger.ts', () => ({
	default: { warn: vi.fn(), info: vi.fn(), error: vi.fn(), debug: vi.fn() },
}));

import { copyText } from './clipboard.ts';
import { invoke } from './tauri.ts';

describe('looksLikeUrl / extRefTarget', () => {
	it('detects http(s) urls', () => {
		expect(looksLikeUrl('https://example.com')).toBe(true);
		expect(looksLikeUrl('D:\\a\\b')).toBe(false);
	});

	it('prefers data-target over href', () => {
		const el = document.createElement('a');
		el.setAttribute('href', 'https://ignored.example');
		el.setAttribute('data-target', 'D:\\real\\path');
		expect(extRefTarget(el)).toBe('D:\\real\\path');
	});
});

describe('handleExtRefEvent', () => {
	beforeEach(() => {
		vi.clearAllMocks();
	});
	afterEach(() => {
		document.body.innerHTML = '';
	});

	function makeLink(target: string) {
		const a = document.createElement('a');
		a.className = EXT_REF_CLASS;
		a.setAttribute('data-target', target);
		a.href = '#';
		a.textContent = target;
		document.body.appendChild(a);
		return a;
	}

	it('copies on plain left click', () => {
		const a = makeLink('https://example.com/x');
		const e = new MouseEvent('click', { bubbles: true, cancelable: true });
		Object.defineProperty(e, 'target', { value: a });
		expect(handleExtRefEvent(e)).toBe(true);
		expect(copyText).toHaveBeenCalledWith('https://example.com/x', '链接');
		expect(invoke).not.toHaveBeenCalled();
	});

	it('copies on right click', () => {
		const a = makeLink('D:\\Workspace\\Haven\\a.rs');
		const e = new MouseEvent('contextmenu', { bubbles: true, cancelable: true });
		Object.defineProperty(e, 'target', { value: a });
		expect(handleExtRefEvent(e)).toBe(true);
		expect(copyText).toHaveBeenCalledWith('D:\\Workspace\\Haven\\a.rs', '路径');
	});

	it('opens on Ctrl+click', () => {
		const a = makeLink('https://example.com');
		const e = new MouseEvent('click', { bubbles: true, cancelable: true, ctrlKey: true });
		Object.defineProperty(e, 'target', { value: a });
		expect(handleExtRefEvent(e)).toBe(true);
		expect(invoke).toHaveBeenCalledWith('open_external', { target: 'https://example.com' });
		expect(copyText).not.toHaveBeenCalled();
	});

	it('returns false when not an ext-ref', () => {
		const span = document.createElement('span');
		document.body.appendChild(span);
		const e = new MouseEvent('click', { bubbles: true, cancelable: true });
		Object.defineProperty(e, 'target', { value: span });
		expect(handleExtRefEvent(e)).toBe(false);
	});
});
