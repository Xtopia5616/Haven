import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import MaterialField from './MaterialField.svelte';

describe('MaterialField', () => {
	it('renders shared label, hint, and validation message', () => {
		render(MaterialField, {
			label: 'Server name',
			forId: 'server-name',
			hint: 'Use a short name',
			error: 'Name is required',
		} as any);

		const label = screen.getByText('Server name');
		expect(label.getAttribute('for')).toBe('server-name');
		expect(screen.getByText('Use a short name')).toBeTruthy();
		expect(screen.getByRole('alert').textContent).toBe('Name is required');
	});
});
