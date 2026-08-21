import { describe, it, expect, vi } from 'vitest';
import { render, fireEvent } from '@testing-library/svelte';
import ApiKeyField from './ApiKeyField.svelte';

describe('ApiKeyField', () => {
	it('shows Set when unconfigured in stored mode', () => {
		const { getByRole, container } = render(ApiKeyField, {
			props: { configured: false },
		});
		expect(getByRole('button', { name: 'Set' })).toBeTruthy();
		expect(container.querySelector('.api-key-mask.grey')).toBeTruthy();
	});

	it('shows Change when configured in stored mode', () => {
		const onEdit = vi.fn();
		const { getByRole } = render(ApiKeyField, {
			props: { configured: true, onEdit },
		});
		const btn = getByRole('button', { name: 'Change' });
		expect(btn).toBeTruthy();
		fireEvent.click(btn);
		expect(onEdit).toHaveBeenCalledTimes(1);
	});

	it('renders a status badge', () => {
		const { container, getByText } = render(ApiKeyField, {
			props: { mode: 'badge', configured: true },
		});
		expect(container.querySelector('.api-key-badge.configured')).toBeTruthy();
		expect(getByText('已配置')).toBeTruthy();
	});

	it('renders an editable password field', () => {
		const { container } = render(ApiKeyField, {
			props: { mode: 'edit', value: '', configured: true },
		});
		const input = container.querySelector('input.api-key-input') as HTMLInputElement;
		expect(input).toBeTruthy();
		expect(input.type).toBe('password');
		expect(input.placeholder).toBe('••••••••••••••••');
	});
});
