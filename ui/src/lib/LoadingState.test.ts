import { render, screen } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import LoadingState from './LoadingState.svelte';

describe('LoadingState', () => {
	it('renders the shared loading animation and accessible status', () => {
		render(LoadingState, {
			label: '正在加载工具…',
			detail: '正在准备工具列表',
		});

		expect(screen.getByRole('status').getAttribute('aria-busy')).toBe('true');
		expect(screen.getByText('正在加载工具…')).toBeTruthy();
		expect(screen.getByText('正在准备工具列表')).toBeTruthy();
		expect(document.querySelectorAll('.loading-state__bar')).toHaveLength(3);
	});

	it('supports the compact inline layout used by the conversation timeline', () => {
		render(LoadingState, { variant: 'inline' });

		expect(document.querySelector('.loading-state--inline')).toBeTruthy();
	});
});
