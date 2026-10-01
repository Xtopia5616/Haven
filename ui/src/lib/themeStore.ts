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

function isStoredAccent(value: string | null): value is string {
	return !!value && (!!ACCENT_PRESETS[value] || /^custom:#[0-9a-f]{6}$/i.test(value));
}

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
	if (isStoredAccent(stored)) return stored;
	if (typeof document === 'undefined') return 'blue';
	const el = document.documentElement;
	const existing = el.getAttribute('data-accent');
	if (isStoredAccent(existing)) return existing;
	return 'blue';
}

function resolveAccentHex(accent: string | null): string {
	if (accent && ACCENT_PRESETS[accent]) return ACCENT_PRESETS[accent].hex;
	if (accent && accent.startsWith(CUSTOM_PREFIX)) {
		const customHex = accent.slice(CUSTOM_PREFIX.length);
		if (/^#[0-9a-f]{6}$/i.test(customHex)) return customHex;
	}
	return '#2C5090';
}

function mixHex(foreground: string, background: string, foregroundWeight: number): string {
	const foregroundChannels = [1, 3, 5].map((offset) =>
		Number.parseInt(foreground.slice(offset, offset + 2), 16),
	);
	const backgroundChannels = [1, 3, 5].map((offset) =>
		Number.parseInt(background.slice(offset, offset + 2), 16),
	);
	const channels = foregroundChannels.map((channel, index) =>
		Math.round(channel * foregroundWeight + backgroundChannels[index] * (1 - foregroundWeight)),
	);
	return '#' + channels.map((channel) => channel.toString(16).padStart(2, '0')).join('');
}

function relativeLuminance(hex: string): number {
	const channels = [1, 3, 5].map((offset) => Number.parseInt(hex.slice(offset, offset + 2), 16) / 255);
	const linear = channels.map((channel) =>
		channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4,
	);
	return linear[0] * 0.2126 + linear[1] * 0.7152 + linear[2] * 0.0722;
}

function contrastingText(hex: string): string {
	const luminance = relativeLuminance(hex);
	const blackContrast = (luminance + 0.05) / 0.05;
	const whiteContrast = 1.05 / (luminance + 0.05);
	return blackContrast >= whiteContrast ? '#000000' : '#ffffff';
}

function applyAccentContrast(accent: string, theme: string) {
	if (typeof document === 'undefined') return;
	const root = document.documentElement;
	const accentHex = resolveAccentHex(accent);
	const dark = theme === 'dark';
	const primary = dark ? mixHex(accentHex, '#ffffff', 0.55) : accentHex;
	const secondary = dark
		? mixHex(accentHex, '#bec6dc', 0.42)
		: mixHex(accentHex, '#545f70', 0.68);
	const tertiary = dark
		? mixHex(accentHex, '#e7b9d1', 0.5)
		: mixHex(accentHex, '#735b6c', 0.6);
	const primaryContainer = dark
		? mixHex(accentHex, '#222631', 0.22)
		: mixHex(accentHex, '#ffffff', 0.2);
	const secondaryContainer = dark
		? mixHex(accentHex, '#3e424c', 0.32)
		: mixHex(accentHex, '#e9edf2', 0.15);
	const tertiaryContainer = dark
		? mixHex(accentHex, '#443c46', 0.26)
		: mixHex(accentHex, '#f0edf0', 0.14);
	root.style.setProperty('--md-accent-on-primary', contrastingText(primary));
	root.style.setProperty('--md-accent-on-secondary', contrastingText(secondary));
	root.style.setProperty('--md-accent-on-tertiary', contrastingText(tertiary));
	root.style.setProperty('--md-accent-on-primary-container', contrastingText(primaryContainer));
	root.style.setProperty('--md-accent-on-secondary-container', contrastingText(secondaryContainer));
	root.style.setProperty('--md-accent-on-tertiary-container', contrastingText(tertiaryContainer));
}

function applyTheme(theme: string) {
	if (typeof document === 'undefined') return;
	document.documentElement.setAttribute('data-theme', theme);
	applyAccentContrast(currentAccent, theme);
}

function applyAccent(accent: string) {
	if (typeof document === 'undefined') return;
	document.documentElement.setAttribute('data-accent', accent);
	document.documentElement.style.setProperty('--md-accent-hex', resolveAccentHex(accent));
	applyAccentContrast(accent, currentTheme);
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
