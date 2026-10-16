import { describe, expect, it } from 'vitest';
import { isValidMcpPackageId } from './mcpPackageId.ts';

describe('isValidMcpPackageId', () => {
	it('matches the administrator catalog ID grammar exactly', () => {
		expect(isValidMcpPackageId('calendar-1')).toBe(true);
		expect(isValidMcpPackageId(`a${'x'.repeat(63)}`)).toBe(true);
		expect(isValidMcpPackageId('')).toBe(false);
		expect(isValidMcpPackageId('1calendar')).toBe(false);
		expect(isValidMcpPackageId('Calendar')).toBe(false);
		expect(isValidMcpPackageId('calendar_mcp')).toBe(false);
		expect(isValidMcpPackageId('calendar\n')).toBe(false);
		expect(isValidMcpPackageId(`a${'x'.repeat(64)}`)).toBe(false);
	});
});
