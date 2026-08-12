import { writable } from 'svelte/store';
import { invoke } from './tauri.js';

const VALID_THEMES = ['light', 'dark'];

const CUSTOM_PREFIX = 'custom:';

const ACCENT_PRESETS = {
	blue: { label: '信息蓝', hex: '#2C5090' },
	green: { label: '邮政绿', hex: '#006548' },
	red: { label: '中国红', hex: '#C82910' },
};

function detectInitialTheme() {
	if (typeof document === 'undefined') return 'dark';
	const el = document.documentElement;
	const existing = el.getAttribute('data-theme');
	if (existing && VALID_THEMES.includes(existing)) return existing;
	const prefersDark =
		typeof window.matchMedia === 'function' && window.matchMedia('(prefers-color-scheme: dark)').matches;
	return prefersDark ? 'dark' : 'light';
}

function detectInitialAccent() {
	if (typeof document === 'undefined') return 'blue';
	const el = document.documentElement;
	const existing = el.getAttribute('data-accent');
	if (existing && (ACCENT_PRESETS[existing] || existing.startsWith(CUSTOM_PREFIX))) return existing;
	return 'blue';
}

function resolveAccentHex(accent) {
	if (ACCENT_PRESETS[accent]) return ACCENT_PRESETS[accent].hex;
	if (accent && accent.startsWith(CUSTOM_PREFIX)) return accent.slice(CUSTOM_PREFIX.length);
	return '#2C5090';
}

function applyTheme(theme) {
	if (typeof document === 'undefined') return;
	document.documentElement.setAttribute('data-theme', theme);
}

function applyAccent(accent) {
	if (typeof document === 'undefined') return;
	document.documentElement.setAttribute('data-accent', accent);
	document.documentElement.style.setProperty('--md-accent-hex', resolveAccentHex(accent));
}

let currentTheme = detectInitialTheme();
let currentAccent = detectInitialAccent();
applyTheme(currentTheme);
applyAccent(currentAccent);

function createStore() {
	const { subscribe, set } = writable({ theme: currentTheme, accent: currentAccent });
	return {
		subscribe,
		get currentTheme() { return currentTheme; },
		get currentAccent() { return currentAccent; },
		get accentColor() { return resolveAccentHex(currentAccent); },
		get presets() { return ACCENT_PRESETS; },
		get isPreset() { return !!ACCENT_PRESETS[currentAccent]; },
		setTheme(theme) {
			if (!VALID_THEMES.includes(theme)) return;
			currentTheme = theme;
			applyTheme(theme);
			set({ theme: currentTheme, accent: currentAccent });
		},
		setAccent(accent) {
			if (!accent) return;
			if (ACCENT_PRESETS[accent] || /^#[0-9a-f]{6}$/i.test(accent)) {
				if (/^#[0-9a-f]{6}$/i.test(accent) && !ACCENT_PRESETS[accent]) {
					accent = CUSTOM_PREFIX + accent;
				}
				currentAccent = accent;
				applyAccent(accent);
				set({ theme: currentTheme, accent: currentAccent });
			}
		},
		toggle() {
			this.setTheme(currentTheme === 'dark' ? 'light' : 'dark');
		},
	};
}

export const themeStore = createStore();

// Persist the current appearance to the backend config (single source of
// truth). Fire-and-forget so a theme toggle never blocks the UI; in a
// non-Tauri context (unit tests) it is a no-op.
export function persistAppearance() {
	invoke('set_appearance', {
		theme: themeStore.currentTheme,
		accent_color: themeStore.currentAccent,
	}).catch(() => {});
}
