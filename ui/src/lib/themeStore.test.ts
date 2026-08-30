import { describe, it, expect, vi, beforeEach } from 'vitest';
import { get } from 'svelte/store';

async function loadThemeStore() {
	vi.resetModules();
	return await import('./themeStore.ts');
}

describe('themeStore', () => {
	beforeEach(() => {
		localStorage.clear();
		document.documentElement.removeAttribute('data-theme');
		document.documentElement.removeAttribute('data-accent');
		document.documentElement.style.removeProperty('--md-accent-hex');
		// jsdom has no matchMedia by default; default to dark via a stub.
		window.matchMedia = vi.fn().mockReturnValue({ matches: true });
	});

	it('detects dark when the OS preference is dark', async () => {
		const { themeStore } = await loadThemeStore();
		expect(themeStore.currentTheme).toBe('dark');
	});

	it('detects light when the OS preference is light', async () => {
		window.matchMedia = vi.fn().mockReturnValue({ matches: false });
		const { themeStore } = await loadThemeStore();
		expect(themeStore.currentTheme).toBe('light');
	});

	it('prefers the data-theme attribute over the OS preference', async () => {
		document.documentElement.setAttribute('data-theme', 'light');
		const { themeStore } = await loadThemeStore();
		expect(themeStore.currentTheme).toBe('light');
	});

	it('prefers persisted localStorage theme over the attribute and OS', async () => {
		localStorage.setItem('haven.theme', 'light');
		localStorage.setItem('haven.accent', 'green');
		document.documentElement.setAttribute('data-theme', 'dark');
		const { themeStore } = await loadThemeStore();
		expect(themeStore.currentTheme).toBe('light');
		expect(themeStore.currentAccent).toBe('green');
	});

	it('persists theme and accent to localStorage on set', async () => {
		const { themeStore } = await loadThemeStore();
		themeStore.setTheme('light');
		themeStore.setAccent('red');
		expect(localStorage.getItem('haven.theme')).toBe('light');
		expect(localStorage.getItem('haven.accent')).toBe('red');
	});

	it('setTheme updates the store and attribute', async () => {
		const { themeStore } = await loadThemeStore();
		themeStore.setTheme('light');
		expect(themeStore.currentTheme).toBe('light');
		expect(get(themeStore).theme).toBe('light');
		expect(document.documentElement.getAttribute('data-theme')).toBe('light');
	});

	it('ignores invalid themes', async () => {
		const { themeStore } = await loadThemeStore();
		themeStore.setTheme('sepia');
		expect(themeStore.currentTheme).toBe('dark');
	});

	it('toggle flips between dark and light', async () => {
		const { themeStore } = await loadThemeStore();
		themeStore.toggle();
		expect(themeStore.currentTheme).toBe('light');
		themeStore.toggle();
		expect(themeStore.currentTheme).toBe('dark');
	});

	it('uses the blue preset accent by default', async () => {
		const { themeStore } = await loadThemeStore();
		expect(themeStore.currentAccent).toBe('blue');
		expect(themeStore.accentColor).toBe('#2C5090');
		expect(themeStore.isPreset).toBe(true);
	});

	it('setAccent with a preset keeps it raw', async () => {
		const { themeStore } = await loadThemeStore();
		themeStore.setAccent('green');
		expect(themeStore.currentAccent).toBe('green');
		expect(themeStore.accentColor).toBe('#006548');
		expect(document.documentElement.getAttribute('data-accent')).toBe('green');
		expect(document.documentElement.style.getPropertyValue('--md-accent-hex')).toBe('#006548');
	});

	it('setAccent with a custom hex stores the custom: prefix', async () => {
		const { themeStore } = await loadThemeStore();
		themeStore.setAccent('#ff0000');
		expect(themeStore.currentAccent).toBe('custom:#ff0000');
		expect(themeStore.accentColor).toBe('#ff0000');
		expect(themeStore.isPreset).toBe(false);
	});

	it('ignores invalid accents', async () => {
		const { themeStore } = await loadThemeStore();
		themeStore.setAccent('purple');
		expect(themeStore.currentAccent).toBe('blue');
		themeStore.setAccent('not-a-color');
		expect(themeStore.currentAccent).toBe('blue');
	});
});
