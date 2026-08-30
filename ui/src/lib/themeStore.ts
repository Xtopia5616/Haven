import { writable } from 'svelte/store';

const VALID_THEMES = ['light', 'dark'];

const CUSTOM_PREFIX = 'custom:';

// localStorage keys — the theme / accent live entirely in the frontend so
// toggles never touch the backend config (no Tauri IPC / config.toml write).
const THEME_KEY = 'haven.theme';
const ACCENT_KEY = 'haven.accent';

// Preset hex values are mirrored in the blocking script in `app.html` for the
// first-paint accent color — keep the two tables in sync when adding/renaming.
const ACCENT_PRESETS: Record<string, { label: string; hex: string }> = {
	blue: { label: '信息蓝', hex: '#2C5090' },
	green: { label: '邮政绿', hex: '#006548' },
	red: { label: '中国红', hex: '#C82910' },
};

function readStorage(key: string): string | null {
	try {
		return window.localStorage.getItem(key);
	} catch {
		return null;
	}
}

function writeStorage(key: string, value: string) {
	try {
		window.localStorage.setItem(key, value);
	} catch {
		// localStorage unavailable (privacy mode etc.) — theme still applies
		// for the current session.
	}
}

function detectInitialTheme(): string {
	const stored = readStorage(THEME_KEY);
	if (stored && VALID_THEMES.includes(stored)) return stored;
	if (typeof document === 'undefined') return 'dark';
	const el = document.documentElement;
	const existing = el.getAttribute('data-theme');
	if (existing && VALID_THEMES.includes(existing)) return existing;
	const prefersDark =
		typeof window.matchMedia === 'function' && window.matchMedia('(prefers-color-scheme: dark)').matches;
	return prefersDark ? 'dark' : 'light';
}

function detectInitialAccent(): string {
	const stored = readStorage(ACCENT_KEY);
	if (stored && (ACCENT_PRESETS[stored] || stored.startsWith(CUSTOM_PREFIX))) return stored;
	if (typeof document === 'undefined') return 'blue';
	const el = document.documentElement;
	const existing = el.getAttribute('data-accent');
	if (existing && (ACCENT_PRESETS[existing] || existing.startsWith(CUSTOM_PREFIX))) return existing;
	return 'blue';
}

function resolveAccentHex(accent: string | null): string {
	if (accent && ACCENT_PRESETS[accent]) return ACCENT_PRESETS[accent].hex;
	if (accent && accent.startsWith(CUSTOM_PREFIX)) return accent.slice(CUSTOM_PREFIX.length);
	return '#2C5090';
}

function applyTheme(theme: string) {
	if (typeof document === 'undefined') return;
	document.documentElement.setAttribute('data-theme', theme);
}

function applyAccent(accent: string) {
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
		setTheme(theme: string) {
			if (!VALID_THEMES.includes(theme)) return;
			currentTheme = theme;
			applyTheme(theme);
			writeStorage(THEME_KEY, theme);
			set({ theme: currentTheme, accent: currentAccent });
		},
		setAccent(accent: string) {
			if (!accent) return;
			if (ACCENT_PRESETS[accent] || /^#[0-9a-f]{6}$/i.test(accent)) {
				if (/^#[0-9a-f]{6}$/i.test(accent) && !ACCENT_PRESETS[accent]) {
					accent = CUSTOM_PREFIX + accent;
				}
				currentAccent = accent;
				applyAccent(accent);
				writeStorage(ACCENT_KEY, accent);
				set({ theme: currentTheme, accent: currentAccent });
			}
		},
		toggle() {
			this.setTheme(currentTheme === 'dark' ? 'light' : 'dark');
		},
	};
}

export const themeStore = createStore();
