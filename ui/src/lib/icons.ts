/**
 * Shared 24px icon registry.
 *
 * The geometry intentionally follows the existing Lucide-like outline style:
 * one viewBox, currentColor, round joins and a consistent default stroke.
 * Keeping the paths here makes size and stroke decisions a single concern.
 */

export type IconDefinition = {
	body: string;
	fill: string;
	stroke: string;
	strokeWidth: number;
	/** Optional optical correction for icons whose path occupies a different amount of the viewBox. */
	opticalScale?: number;
};

const outline = (body: string, strokeWidth = 2, opticalScale = 1): IconDefinition => ({
	body,
	fill: 'none',
	stroke: 'currentColor',
	strokeWidth,
	opticalScale,
});

const filled = (body: string): IconDefinition => ({
	body,
	fill: 'currentColor',
	stroke: 'none',
	strokeWidth: 0,
});

export const ICONS = {
	activity: outline('<polyline points="22 12 18 12 15 21 9 3 6 12 2 12" />'),
	alertCircle: filled(
		'<path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm1 15h-2v-6h2v6zm0-8h-2V7h2v2z" />',
	),
	alertTriangle: filled('<path d="M1 21h22L12 2 1 21zm13-3h-4v-2h4v2zm0-4h-4v-4h4v4z" />'),
	arrowDown: outline('<path d="M12 5v14" /><polyline points="19 12 12 19 5 12" />'),
	agent: outline(
		'<path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2" /><circle cx="9" cy="7" r="4" /><path d="M23 21v-2a4 4 0 0 0-3-3.87M16 3.13a4 4 0 0 1 0 7.75" />',
	),
	branch: outline(
		'<circle cx="6" cy="6" r="3" /><circle cx="6" cy="18" r="3" /><path d="M6 9v6M18 9h-6a4 4 0 0 0-4 4v4" /><circle cx="18" cy="6" r="3" />',
	),
	briefcase: outline(
		'<path d="M4 9h16v10.5H4zM8 9V6.75A1.75 1.75 0 0 1 9.75 5h4.5A1.75 1.75 0 0 1 16 6.75V9M4 13h16M10 13v2h4v-2" />',
	),
	calendar: outline(
		'<rect x="3" y="4" width="18" height="18" rx="2" /><line x1="16" y1="2" x2="16" y2="6" /><line x1="8" y1="2" x2="8" y2="6" /><line x1="3" y1="10" x2="21" y2="10" />',
	),
	check: outline('<polyline points="20 6 9 17 4 12" />'),
	checkCircle: filled(
		'<path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm-2 15-5-5 1.41-1.41L10 14.17l7.59-7.59L19 8l-9 9z" />',
	),
	chevronDown: outline('<polyline points="6 9 12 15 18 9" />'),
	chevronLeft: outline('<path d="M15 18l-6-6 6-6" />'),
	chevronRight: outline('<path d="M9 18l6-6-6-6" />'),
	chevronUp: outline('<path d="M18 15l-6-6-6 6" />'),
	chat: outline(
		'<path d="M5.5 4.5h10A3.5 3.5 0 0 1 19 8v4.25a3.5 3.5 0 0 1-3.5 3.5H11l-5.5 4v-4.04a3.5 3.5 0 0 1-3.5-3.46V8a3.5 3.5 0 0 1 3.5-3.5Z" /><path d="M7 9h7M7 12h4" />',
		2,
		0.95,
	),
	clock: outline('<circle cx="12" cy="12" r="9" /><polyline points="12 7 12 12 15.5 13.5" />'),
	close: outline('<path d="m6 6 12 12M18 6 6 18" />'),
	clipboard: outline(
		'<path d="M16 4h2a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2" /><rect x="8" y="2" width="8" height="4" rx="1" />',
	),
	copy: outline(
		'<rect x="9" y="9" width="13" height="13" rx="2" /><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />',
	),
	cpu: outline(
		'<rect x="5" y="5" width="14" height="14" rx="2" /><rect x="9.5" y="9.5" width="5" height="5" rx="1" />',
	),
	cut: outline(
		'<circle cx="6" cy="6" r="3" /><circle cx="6" cy="18" r="3" /><line x1="20" y1="4" x2="8.12" y2="15.88" /><line x1="14.47" y1="14.48" x2="20" y2="20" /><line x1="8.12" y1="8.12" x2="12" y2="12" />',
	),
	delete: outline(
		'<path d="M3 6h18" /><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6" /><path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" />',
	),
	download: outline(
		'<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" /><polyline points="7 10 12 15 17 10" /><line x1="12" y1="15" x2="12" y2="3" />',
	),
	export: outline(
		'<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" /><polyline points="7 10 12 15 17 10" /><line x1="12" y1="15" x2="12" y2="3" />',
	),
	edit: outline('<path d="M17 3a2.85 2.85 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5Z" />'),
	eye: outline(
		'<path d="M2 12s3.5-6 10-6 10 6 10 6-3.5 6-10 6S2 12 2 12Z" /><circle cx="12" cy="12" r="2.5" />',
	),
	eyeOff: outline(
		'<path d="m3 3 18 18M10.6 10.6a2 2 0 0 0 2.8 2.8M9.9 4.3A10.7 10.7 0 0 1 12 4c6.5 0 10 8 10 8a18.5 18.5 0 0 1-3.2 4.5M6.2 6.2C3.6 8.2 2 12 2 12s3.5 8 10 8c1.5 0 2.9-.4 4.1-1" />',
	),
	file: outline(
		'<path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" /><polyline points="14 2 14 8 20 8" />',
	),
	fileSearch: outline(
		'<path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" /><polyline points="14 2 14 8 20 8" /><circle cx="10.5" cy="14.5" r="2.5" /><path d="m12.5 16.5 2 2" />',
	),
	globe: outline(
		'<circle cx="12" cy="12" r="10" /><path d="M2 12h20M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z" />',
	),
	help: outline(
		'<circle cx="12" cy="12" r="9" /><path d="M9.5 9a2.5 2.5 0 1 1 4.3 1.75c-.9.9-1.8 1.25-1.8 2.75M12 17h.01" />',
	),
	history: outline(
		'<path d="M3 12a9 9 0 1 0 3-6.7" /><path d="M3 5v5h5" /><path d="M12 7v5l3 2" />',
		2,
		0.9,
	),
	info: filled(
		'<path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm1 15h-2v-6h2v6zm0-8h-2V7h2v2z" />',
	),
	key: outline('<circle cx="7.5" cy="15.5" r="3.5" /><path d="m10 13 8-8 3 3-2 2m-3-3 2 2" />'),
	listTodo: outline(
		'<rect x="4" y="3.5" width="16" height="17" rx="3" /><path d="M8 8h.01M11.5 8H16M8 12h.01M11.5 12H16M8 16h.01M11.5 16H14" />',
		1.8,
	),
	memory: outline(
		'<path d="M12 3c4.4 0 8 1.8 8 4s-3.6 4-8 4-8-1.8-8-4 3.6-4 8-4Z" /><path d="M4 7v5c0 2.2 3.6 4 8 4s8-1.8 8-4V7M4 12v5c0 2.2 3.6 4 8 4s8-1.8 8-4v-5" />',
	),
	mic: filled(
		'<path d="M12 14c1.66 0 3-1.34 3-3V5c0-1.66-1.34-3-3-3S9 3.34 9 5v6c0 1.66 1.34 3 3 3zm5.91-3c-.49 0-.9.36-.98.85C16.52 14.2 14.47 16 12 16s-4.52-1.8-4.93-4.15c-.08-.49-.49-.85-.98-.85-.61 0-1.09.54-1 1.14.49 3 2.89 5.35 5.91 5.78V20c0 .55.45 1 1 1s1-.45 1-1v-2.08c3.02-.43 5.42-2.78 5.91-5.78.1-.6-.39-1.14-1-1.14z" />',
	),
	minus: outline('<path d="M5 12h14" />'),
	moon: filled('<path d="M21 12.8A9 9 0 1111.2 3a7 7 0 009.8 9.8z" />'),
	monitor: outline(
		'<rect x="2" y="3" width="20" height="14" rx="2" /><path d="M8 21h8M12 17v4" />',
	),
	network: outline(
		'<circle cx="6" cy="12" r="2" /><circle cx="18" cy="6" r="2" /><circle cx="18" cy="18" r="2" /><path d="m7.7 11 8.6-4M7.7 13l8.6 4" />',
	),
	open: outline(
		'<path d="M15 3h6v6M10 14 21 3M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" />',
	),
	paperclip: outline(
		'<path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" />',
	),
	paste: outline(
		'<path d="M16 4h2a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2" /><rect x="8" y="2" width="8" height="4" rx="1" />',
	),
	pause: outline(
		'<rect x="6" y="4" width="4" height="16" /><rect x="14" y="4" width="4" height="16" />',
	),
	play: outline('<polygon points="5 3 19 12 5 21 5 3" />'),
	plus: outline('<path d="M12 5v14M5 12h14" />'),
	power: outline('<path d="M12 2v10M18.4 6.6a9 9 0 1 1-12.77.04" />'),
	refresh: outline(
		'<polyline points="23 4 23 10 17 10" /><path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10" />',
	),
	rollback: outline(
		'<polyline points="1 4 1 10 7 10" /><path d="M3.51 15a9 9 0 1 0 2.13-9.36L1 10" />',
	),
	search: outline(
		'<circle cx="11" cy="11" r="7" /><line x1="21" y1="21" x2="16.65" y2="16.65" />',
	),
	selectAll: outline(
		'<rect x="3" y="3" width="18" height="18" rx="2" /><path d="M7 8h10M7 12h10M7 16h6" />',
	),
	send: outline('<line x1="12" y1="19" x2="12" y2="5" /><polyline points="5 12 12 5 19 12" />'),
	settings: outline(
		'<path d="M9.5 3.5A2.5 2.5 0 0 1 12 1a2.5 2.5 0 0 1 2.5 2.5v.18a2 2 0 0 0 1 1.73l.43.25a2 2 0 0 0 2 0l.15-.08a2.5 2.5 0 0 1 3.41.91l.22.38a2.5 2.5 0 0 1-.91 3.41l-.15.09a2 2 0 0 0-1 1.74v.5a2 2 0 0 0 1 1.74l.15.09a2.5 2.5 0 0 1 .91 3.41l-.22.38a2.5 2.5 0 0 1-3.41.91l-.15-.08a2 2 0 0 0-2 0l-.43.25a2 2 0 0 0-1 1.73v.18A2.5 2.5 0 0 1 12 23a2.5 2.5 0 0 1-2.5-2.5v-.18a2 2 0 0 0-1-1.73l-.43-.25a2 2 0 0 0-2 0l-.15.08a2.5 2.5 0 0 1-3.41-.91l-.22-.38a2.5 2.5 0 0 1 .91-3.41l.15-.09a2 2 0 0 0 1-1.74v-.5a2 2 0 0 0-1-1.74L3.2 9.55a2.5 2.5 0 0 1-.91-3.41l.22-.38a2.5 2.5 0 0 1 3.41-.91l.15.08a2.5 2.5 0 0 0 2 0l.43-.25a2 2 0 0 0 1-1.73Z" /><circle cx="12" cy="12" r="3.25" />',
		2,
		0.8,
	),
	sparkles: outline(
		'<path d="m12 3-1.8 5.2L5 10l5.2 1.8L12 17l1.8-5.2L19 10l-5.2-1.8L12 3Z" /><path d="m19 16-.7 2.3L16 19l2.3.7L19 22l.7-2.3L22 19l-2.3-.7L19 16Z" />',
	),
	stop: outline('<rect x="4" y="4" width="16" height="16" rx="2" />'),
	sun: filled(
		'<path d="M12 7a5 5 0 100 10 5 5 0 000-10zm0-5a1 1 0 011 1v2a1 1 0 11-2 0V3a1 1 0 011-1zm0 17a1 1 0 011 1v2a1 1 0 11-2 0v-2a1 1 0 011-1zM4.2 4.2a1 1 0 011.4 0l1.5 1.5A1 1 0 015.7 7.1L4.2 5.6a1 1 0 010-1.4zm12.7 12.7a1 1 0 011.4 0l1.5 1.5a1 1 0 11-1.4 1.4l-1.5-1.5a1 1 0 010-1.4zM2 12a1 1 0 011-1h2a1 1 0 110 2H3a1 1 0 01-1-1zm17 0a1 1 0 011-1h2a1 1 0 110 2h-2a1 1 0 01-1-1zM4.2 19.8a1 1 0 010-1.4l1.5-1.5a1 1 0 111.4 1.4l-1.5 1.5a1 1 0 01-1.4 0zm12.7-12.7a1 1 0 010-1.4l1.5-1.5a1 1 0 111.4 1.4l-1.5 1.5a1 1 0 01-1.4 0z" />',
	),
	terminal: outline(
		'<polyline points="4 17 10 11 4 5" /><line x1="12" y1="19" x2="20" y2="19" />',
	),
	tools: outline(
		'<path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z" />',
	),
	users: outline(
		'<path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2" /><circle cx="9" cy="7" r="4" /><path d="M23 21v-2a4 4 0 0 0-3-3.87M16 3.13a4 4 0 0 1 0 7.75" />',
	),
	window: outline(
		'<rect x="2" y="3" width="20" height="14" rx="2" /><path d="M8 21h8M12 17v4" />',
	),
	xCircle: filled(
		'<path d="M12 2C6.47 2 2 6.47 2 12s4.47 10 10 10 10-4.47 10-10S17.53 2 12 2zm5 13.59L15.59 17 12 13.41 8.41 17 7 15.59 10.59 12 7 8.41 8.41 7 12 10.59 15.59 7 17 8.41 13.41 12 17 15.59z" />',
	),
} as const satisfies Record<string, IconDefinition>;

export type IconName = keyof typeof ICONS;

export function getIconDefinition(name: string | undefined): IconDefinition {
	return ICONS[name as IconName] ?? ICONS.help;
}

export function hasIcon(name: unknown): name is IconName {
	return typeof name === 'string' && name in ICONS;
}

export function getIconTransform(definition: IconDefinition): string | undefined {
	const scale = definition.opticalScale ?? 1;
	if (scale === 1) return undefined;
	const offset = 12 * (1 - scale);
	return `translate(${offset} ${offset}) scale(${scale})`;
}

/**
 * Used only by static, application-owned HTML renderers such as markdown's
 * copy button. User-authored content never reaches this registry.
 */
export function renderIconSvg(name: string, size = 20): string {
	const definition = getIconDefinition(name);
	const transform = getIconTransform(definition);
	const body = transform ? `<g transform="${transform}">${definition.body}</g>` : definition.body;
	return `<svg width="${size}" height="${size}" viewBox="0 0 24 24" fill="${definition.fill}" stroke="${definition.stroke}" stroke-width="${definition.strokeWidth}" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${body}</svg>`;
}
