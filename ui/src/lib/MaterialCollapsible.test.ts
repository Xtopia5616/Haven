import { describe, it, expect } from 'vitest';
import { render, fireEvent } from '@testing-library/svelte';
import MaterialCollapsible from './MaterialCollapsible.svelte';

describe('MaterialCollapsible', () => {
	it('keeps the body mounted but hidden when collapsed (default)', () => {
		const { container } = render(MaterialCollapsible, { open: false });
		const header = container.querySelector('.md-collapsible-header') as HTMLButtonElement;
		expect(header).toBeTruthy();
		expect(header.getAttribute('aria-expanded')).toBe('false');
		expect(container.querySelector('.md-collapsible-caret')).toBeTruthy();
		expect(container.querySelector('.md-collapsible-body')?.hasAttribute('hidden')).toBe(true);
	});

	it('toggles open when the header is clicked', async () => {
		const { container } = render(MaterialCollapsible, { open: false });
		const header = container.querySelector('.md-collapsible-header') as HTMLButtonElement;
		await fireEvent.click(header);
		expect(header.getAttribute('aria-expanded')).toBe('true');
		expect(container.querySelector('.md-collapsible-body')?.hasAttribute('hidden')).toBe(false);
	});

	it('starts expanded when open is true', () => {
		const { container } = render(MaterialCollapsible, { open: true });
		const header = container.querySelector('.md-collapsible-header') as HTMLButtonElement;
		expect(header.getAttribute('aria-expanded')).toBe('true');
		expect(container.querySelector('.md-collapsible')?.getAttribute('data-open')).toBe('true');
	});

	it('unmounts the body while collapsed when lazy', () => {
		const { container } = render(MaterialCollapsible, { open: false, lazy: true });
		expect(container.querySelector('.md-collapsible-body')).toBeNull();
	});

	it('mounts the lazy body when opened', async () => {
		const { container } = render(MaterialCollapsible, { open: false, lazy: true });
		const header = container.querySelector('.md-collapsible-header') as HTMLButtonElement;
		await fireEvent.click(header);
		expect(container.querySelector('.md-collapsible-body')).toBeTruthy();
	});

	it('applies the error variant', () => {
		const { container } = render(MaterialCollapsible, {
			open: false,
			variant: 'error' as const,
		});
		expect(container.querySelector('.md-collapsible')?.getAttribute('data-variant')).toBe(
			'error',
		);
	});
});
