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
		expect(screen.getByRole('status').getAttribute('aria-label')).toBe(
			'正在加载工具…，正在准备工具列表',
		);
		expect(screen.queryByText('正在加载工具…')).toBeNull();
		expect(screen.queryByText('正在准备工具列表')).toBeNull();
		expect(document.querySelectorAll('.voice-bars--float .voice-bars__bar')).toHaveLength(3);
		expect(document.querySelector('.loading-state--page')).toBeTruthy();
	});

	it('supports the compact inline layout used by the conversation timeline', () => {
		render(LoadingState, { variant: 'inline' });

		expect(document.querySelector('.loading-state--inline')).toBeTruthy();
	});
});
