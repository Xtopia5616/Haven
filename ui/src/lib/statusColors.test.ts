import { describe, expect, it } from 'vitest';
import { getStatusColorTokens, getStatusDotColor, resolveStatusTone } from './statusColors.ts';

describe('statusColors', () => {
	it('keeps the status vocabulary semantic and theme-aware', () => {
		expect(getStatusDotColor('success')).toBe('var(--md-sys-color-success)');
		expect(getStatusDotColor('warning')).toBe('var(--md-sys-color-warning)');
		expect(getStatusDotColor('error')).toBe('var(--md-sys-color-error)');
		expect(getStatusDotColor('info')).toBe('var(--md-sys-color-primary)');
		expect(getStatusDotColor('outline')).toBe('var(--md-sys-color-outline)');
	});

	it('maps existing component aliases to the same standard tones', () => {
		expect(resolveStatusTone('primary')).toBe('info');
		expect(resolveStatusTone('tertiary')).toBe('tool');
		expect(resolveStatusTone('outline')).toBe('neutral');
		expect(getStatusColorTokens('error').background).toBe(
			'var(--md-sys-color-error-container)',
		);
	});
});
