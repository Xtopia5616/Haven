import { describe, expect, it } from 'vitest';
import {
	declaredResourceDescription,
	permissionResourceScopeDescription,
} from './permissionResourceDescription.ts';

describe('permission resource descriptions', () => {
	it('distinguishes clipboard read and write resources', () => {
		expect(
			declaredResourceDescription({ kind: 'clipboard', access: 'read' }),
		).toBe('读取系统剪贴板');
		expect(
			permissionResourceScopeDescription({ kind: 'clipboard', access: 'write' }),
		).toBe('写入系统剪贴板');
	});

	it('shows the exact managed asset identity', () => {
		expect(
			declaredResourceDescription({
				kind: 'managed_asset',
				asset_id: 'asset-0123456789abcdef0123456789abcdef',
			}),
		).toBe('读取受管媒体资产 · asset-0123456789abcdef0123456789abcdef');
	});
});
