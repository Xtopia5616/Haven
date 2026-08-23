import { describe, expect, it } from 'vitest';
import { ROLE_KEYS, emptyRoleSlot, ensureRoleSlots } from './modelRoles.ts';

describe('ensureRoleSlots', () => {
	it('appends missing role slots without touching existing ones', () => {
		const roles = [{ role: 'default_model', provider: 'p', model: 'm' }];
		ensureRoleSlots(roles);
		expect(roles).toHaveLength(ROLE_KEYS.length);
		expect(roles[0]).toEqual({ role: 'default_model', provider: 'p', model: 'm' });
		for (const key of ROLE_KEYS) {
			expect(roles.some((r) => r.role === key)).toBe(true);
		}
	});

	it('is a no-op when every role already exists', () => {
		const roles = ROLE_KEYS.map((key) => emptyRoleSlot(key));
		const before = JSON.stringify(roles);
		ensureRoleSlots(roles);
		expect(JSON.stringify(roles)).toBe(before);
	});
});
