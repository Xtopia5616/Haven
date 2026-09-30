import { render, screen } from '@testing-library/svelte';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import LoadingState from './LoadingState.svelte';
import loadingStateSource from './LoadingState.svelte?raw';
import appHtmlSource from '../app.html?raw';

const componentStyles = loadingStateSource.match(/<style>([\s\S]*?)<\/style>/)?.[1];
const testStyleElement = document.createElement('style');

beforeAll(() => {
	if (!componentStyles) throw new Error('LoadingState component styles are missing');
	testStyleElement.textContent = componentStyles;
	document.head.append(testStyleElement);
});

afterAll(() => testStyleElement.remove());

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
		const pageLoader = document.querySelector<HTMLElement>('.loading-state--page');
		expect(pageLoader).toBeTruthy();
		const pageLoaderStyle = getComputedStyle(pageLoader!);
		expect(pageLoaderStyle.position).toBe('static');
		expect(pageLoaderStyle.pointerEvents).toBe('auto');
	});

	it('covers the full window only for the initial app boot loader', () => {
		const bootLoaderStyles = appHtmlSource.match(/#haven-boot\s*\{([^}]*)\}/)?.[1];
		expect(bootLoaderStyles).toBeTruthy();
		expect(bootLoaderStyles).toMatch(/position:\s*fixed\s*;/);
		expect(bootLoaderStyles).toMatch(/inset:\s*0\s*;/);
	});

	it('supports the compact inline layout for opt-in embedded surfaces', () => {
		render(LoadingState, { variant: 'inline' });

		expect(document.querySelector('.loading-state--inline')).toBeTruthy();
	});
});
