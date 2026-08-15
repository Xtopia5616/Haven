import { describe, it, expect } from 'vitest';
import { ACTION_STATUSES, statusColor, statusVariant } from './sessionStatus.js';

describe('ACTION_STATUSES', () => {
	it('covers the canonical backend statuses', () => {
		expect(ACTION_STATUSES).toEqual(['pending', 'running', 'paused', 'completed', 'failed', 'error']);
	});
});

describe('statusColor', () => {
	it('maps every status to its hex color', () => {
		expect(statusColor('pending')).toBe('#666');
		expect(statusColor('running')).toBe('#44cc44');
		expect(statusColor('paused')).toBe('#ccaa44');
		expect(statusColor('completed')).toBe('#4488ff');
		expect(statusColor('failed')).toBe('#ff4444');
		expect(statusColor('error')).toBe('#ff4444');
	});

	it('falls back to the pending gray for unknown statuses', () => {
		expect(statusColor('paused_pending')).toBe('#666');
		expect(statusColor('')).toBe('#666');
		expect(statusColor(undefined)).toBe('#666');
	});
});

describe('statusVariant', () => {
	it('maps every status to its MaterialBadge variant', () => {
		expect(statusVariant('pending')).toBe('default');
		expect(statusVariant('running')).toBe('primary');
		expect(statusVariant('paused')).toBe('warning');
		expect(statusVariant('completed')).toBe('success');
		expect(statusVariant('failed')).toBe('error');
		expect(statusVariant('error')).toBe('error');
	});

	it('falls back to default for unknown statuses', () => {
		expect(statusVariant('paused_pending')).toBe('default');
		expect(statusVariant(undefined)).toBe('default');
	});
});
