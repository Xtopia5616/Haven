import { describe, expect, it } from 'vitest';
import { getStatusColorTokens, getStatusDotColor, resolveStatusTone } from './statusColors.ts';

describe('statusColors', () => {
	it('keeps the status vocabulary semantic and theme-aware', () => {
		expect(getStatusDotColor('success')).toBe('var(--md-sys-color-success)');
		expect(getStatusDotColor('warning')).toBe('var(--md-sys-color-warning)');
		expect(getStatusDotColor('error')).toBe('var(--md-sys-color-error)');
		expect(getStatusDotColor('info')).toBe('var(--md-sys-color-primary)');
		expect(getStatusDotColor('neutral')).toBe('var(--md-sys-color-outline)');
	});

	it('uses neutral for unknown values', () => {
		expect(resolveStatusTone('unknown')).toBe('neutral');
		expect(getStatusColorTokens('error').background).toBe(
			'var(--md-sys-color-error-container)',
		);
	});
});
