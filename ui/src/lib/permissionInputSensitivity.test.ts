import { describe, expect, it } from 'vitest';
import { isSensitivePermissionInputValue } from './permissionInputSensitivity.ts';

describe('permission input sensitivity detection', () => {
	it.each(['api_key', 'OPENAI_API_KEY', 'authorization', 'password', 'clientSecret'])(
		'folds values under credential-like field %s',
		(key) => {
			expect(isSensitivePermissionInputValue(key, 'example-value-123')).toBe(true);
		},
	);

	it.each([
		'curl https://example.test/data?token=example-value-123',
		'curl -H "Authorization: Bearer example-value-123" https://example.test',
		'curl --api-key example-value-123 https://example.test',
		'https://user:example-value-123@example.test/private',
	])('folds inline credentials in command values', (value) => {
		expect(isSensitivePermissionInputValue('command', value)).toBe(true);
	});

	it('keeps ordinary command values visible', () => {
		expect(isSensitivePermissionInputValue('command', 'cargo test --workspace')).toBe(false);
		expect(isSensitivePermissionInputValue('max_tokens', 1200)).toBe(false);
	});
});
