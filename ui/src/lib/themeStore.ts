import { get, writable } from 'svelte/store';
import { isOneOf } from './contracts/valueGuards.ts';

const VALID_THEMES = ['light', 'dark'] as const;
export type ThemeMode = (typeof VALID_THEMES)[number];

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

function isThemeMode(value: unknown): value is ThemeMode {
	return isOneOf(value, VALID_THEMES);
}

function detectInitialTheme(): ThemeMode {
	const stored = readStorage(THEME_KEY);
	if (isThemeMode(stored)) return stored;
	if (typeof document === 'undefined') return 'dark';
	const el = document.documentElement;
	const existing = el.getAttribute('data-theme');
	if (isThemeMode(existing)) return existing;
	const prefersDark =
		typeof window.matchMedia === 'function' &&
		window.matchMedia('(prefers-color-scheme: dark)').matches;
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
	const channels = [1, 3, 5].map(
		(offset) => Number.parseInt(hex.slice(offset, offset + 2), 16) / 255,
	);
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

function applyAccentContrast(accent: string, theme: ThemeMode) {
	if (typeof document === 'undefined') return;
	const root = document.documentElement;
	const accentHex = resolveAccentHex(accent);
	const dark = theme === 'dark';
	const primary = dark ? mixHex(accentHex, '#ffffff', 0.55) : accentHex;
	const primaryContainer = dark
		? mixHex(accentHex, '#232b37', 0.24)
		: mixHex(accentHex, '#ffffff', 0.12);
	const containerInk = dark ? '#e7ecf3' : '#253044';
	const inkLuminance = relativeLuminance(containerInk);
	const backgroundLuminance = relativeLuminance(primaryContainer);
	const containerContrast =
		(Math.max(inkLuminance, backgroundLuminance) + 0.05) /
		(Math.min(inkLuminance, backgroundLuminance) + 0.05);
	root.style.setProperty('--md-accent-on-primary', contrastingText(primary));
	root.style.setProperty(
		'--md-accent-on-primary-container',
		containerContrast >= 4.5 ? containerInk : contrastingText(primaryContainer),
	);
}

function applyTheme(theme: ThemeMode, accent: string) {
	if (typeof document === 'undefined') return;
	document.documentElement.setAttribute('data-theme', theme);
	applyAccentContrast(accent, theme);
}

function applyAccent(accent: string, theme: ThemeMode) {
	if (typeof document === 'undefined') return;
	document.documentElement.setAttribute('data-accent', accent);
	document.documentElement.style.setProperty('--md-accent-hex', resolveAccentHex(accent));
	applyAccentContrast(accent, theme);
}

function createStore() {
	const state = writable({ theme: detectInitialTheme(), accent: detectInitialAccent() });
	const initial = get(state);
	applyTheme(initial.theme, initial.accent);
	applyAccent(initial.accent, initial.theme);

	return {
		subscribe: state.subscribe,
		get currentTheme() {
			return get(state).theme;
		},
		get currentAccent() {
			return get(state).accent;
		},
		get accentColor() {
			return resolveAccentHex(get(state).accent);
		},
		get presets() {
			return ACCENT_PRESETS;
		},
		get isPreset() {
			return !!ACCENT_PRESETS[get(state).accent];
		},
		setTheme(theme: ThemeMode) {
			if (!isThemeMode(theme)) return;
			const { accent } = get(state);
			applyTheme(theme, accent);
			writeStorage(THEME_KEY, theme);
			state.set({ theme, accent });
		},
		setAccent(accent: string) {
			if (!accent) return;
			if (ACCENT_PRESETS[accent] || /^#[0-9a-f]{6}$/i.test(accent)) {
				if (/^#[0-9a-f]{6}$/i.test(accent) && !ACCENT_PRESETS[accent]) {
					accent = CUSTOM_PREFIX + accent;
				}
				const { theme } = get(state);
				applyAccent(accent, theme);
				writeStorage(ACCENT_KEY, accent);
				state.set({ theme, accent });
			}
		},
		toggle() {
			this.setTheme(get(state).theme === 'dark' ? 'light' : 'dark');
		},
	};
}

export const themeStore = createStore();
