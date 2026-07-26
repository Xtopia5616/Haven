import { writable } from 'svelte/store';

const THEME_KEY = 'haven.theme';
const ACCENT_KEY = 'haven.accent';
const VALID_THEMES = ['light', 'dark'];

const CUSTOM_PREFIX = 'custom:';

const ACCENT_PRESETS = {
	blue: { label: '信息蓝', hex: '#2C5090' },
	green: { label: '邮政绿', hex: '#006548' },
	red: { label: '中国红', hex: '#C82910' },
};

function detectInitialTheme() {
	if (typeof window === 'undefined') return 'dark';
	const el = document.documentElement;
	const existing = el.getAttribute('data-theme');
	if (existing && VALID_THEMES.includes(existing)) return existing;
	try {
		const saved = window.localStorage.getItem(THEME_KEY);
		if (saved && VALID_THEMES.includes(saved)) return saved;
	} catch {}
	const prefersDark =
		typeof window.matchMedia === 'function' && window.matchMedia('(prefers-color-scheme: dark)').matches;
	return prefersDark ? 'dark' : 'light';
}

function detectInitialAccent() {
	if (typeof window === 'undefined') return 'blue';
	const el = document.documentElement;
	const existing = el.getAttribute('data-accent');
	if (existing && (ACCENT_PRESETS[existing] || existing.startsWith(CUSTOM_PREFIX))) return existing;
	try {
		const saved = window.localStorage.getItem(ACCENT_KEY);
		if (saved && (ACCENT_PRESETS[saved] || saved.startsWith(CUSTOM_PREFIX))) return saved;
	} catch {}
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
			try { window.localStorage.setItem(THEME_KEY, theme); } catch {}
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
				try { window.localStorage.setItem(ACCENT_KEY, accent); } catch {}
				set({ theme: currentTheme, accent: currentAccent });
			}
		},
		toggle() {
			this.setTheme(currentTheme === 'dark' ? 'light' : 'dark');
		},
	};
}

export const themeStore = createStore();
