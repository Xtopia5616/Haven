import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';

const themeCss = readFileSync('src/app.css', 'utf8');

function luminance(hex: string): number {
	const linear = [1, 3, 5].map((offset) => {
		const channel = Number.parseInt(hex.slice(offset, offset + 2), 16) / 255;
		return channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
	});
	return linear[0] * 0.2126 + linear[1] * 0.7152 + linear[2] * 0.0722;
}

// Read the shipped CSS rather than maintaining a second palette in the test.
describe.each(['light', 'dark'])('%s palette readability', (theme) => {
	const block = themeCss.match(new RegExp(`\\[data-theme='${theme}'\\] \\{([^}]+)\\}`))![1];
	const colors = Object.fromEntries(
		[...block.matchAll(/--md-sys-color-([\w-]+): (#[\da-f]{6});/g)].map((match) => [
			match[1],
			match[2],
		]),
	);
	const pairs = [
		['on-surface', 'background'],
		['on-surface', 'surface-container-highest'],
		['on-surface-variant', 'background'],
		['on-surface-variant', 'surface-container-highest'],
		...['secondary', 'tertiary', 'error', 'success', 'warning'].flatMap((role) => [
			[role, 'background'],
			[`on-${role}-container`, `${role}-container`],
		]),
		['on-secondary', 'secondary'],
		['on-tertiary', 'tertiary'],
		['on-error', 'error'],
	];

	it.each(pairs)('%s on %s meets 4.5:1 text contrast', (foreground, background) => {
		const foregroundLuminance = luminance(colors[foreground]);
		const backgroundLuminance = luminance(colors[background]);
		const contrast =
			(Math.max(foregroundLuminance, backgroundLuminance) + 0.05) /
			(Math.min(foregroundLuminance, backgroundLuminance) + 0.05);
		expect(contrast).toBeGreaterThanOrEqual(4.5);
	});
});
