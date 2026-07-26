import { writable } from 'svelte/store';
import { invoke } from './tauri.js';

/* ===========================================================================
 * Theme store — Material 3 expressive, light/dark only (manual quick toggle).
 * Persistence: localStorage('haven.theme'); applies [data-theme] on <html>.
 * ========================================================================= */

const THEME_KEY = 'haven.theme';
const VALID = ['light', 'dark'];

function detectInitial() {
	if (typeof window === 'undefined') return 'dark';
	const el = document.documentElement;
	const existing = el.getAttribute('data-theme');
	if (existing && VALID.includes(existing)) return existing;
	try {
		const saved = window.localStorage.getItem(THEME_KEY);
		if (saved && VALID.includes(saved)) return saved;
	} catch (e) {
		console.warn('[theme] localStorage getItem failed:', e);
	}
	const prefersDark =
		typeof window.matchMedia === 'function' && window.matchMedia('(prefers-color-scheme: dark)').matches;
	return prefersDark ? 'dark' : 'light';
}

function apply(theme) {
	if (typeof document === 'undefined') return;
	document.documentElement.setAttribute('data-theme', theme);
}

let current = detectInitial();
apply(current);

function createStore() {
	const { subscribe, set } = writable(current);
	return {
		subscribe,
		get current() {
			return current;
		},
		set(theme) {
			if (!VALID.includes(theme)) return;
			current = theme;
			apply(theme);
			try {
				window.localStorage.setItem(THEME_KEY, theme);
			} catch (e) {
				console.warn('[theme] localStorage setItem failed:', e);
			}
			// Best-effort: persist to backend settings so other windows follow.
			try {
				invoke('update_appearance', { theme });
			} catch (e) {
				console.warn('[theme] update_appearance failed:', e);
			}set(theme);
		},
		toggle() {
			this.set(current === 'dark' ? 'light' : 'dark');
		},
	};
}

export const themeStore = createStore();