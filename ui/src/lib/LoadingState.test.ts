import { render, screen } from '@testing-library/svelte';
import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest';
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
		expect(pageLoaderStyle.flexGrow).toBe('1');
		expect(pageLoaderStyle.pointerEvents).toBe('auto');
	});

	it('covers the full window only for the initial app boot loader', () => {
		const bootLoaderStyles = appHtmlSource.match(/#haven-boot\s*\{([^}]*)\}/)?.[1];
		expect(bootLoaderStyles).toBeTruthy();
		expect(bootLoaderStyles).toMatch(/position:\s*fixed\s*;/);
		expect(bootLoaderStyles).toMatch(/inset:\s*0\s*;/);
	});

	it('shows a retry fallback when the initial app boot does not complete', () => {
		expect(appHtmlSource).toContain('id="haven-boot-error"');
		expect(appHtmlSource).toContain('Haven 启动遇到问题');
		expect(appHtmlSource).toContain('id="haven-boot-retry"');
		expect(appHtmlSource).toContain('window.location.reload()');
		expect(appHtmlSource).toMatch(/\},\s*30000\);/);
	});

	it('changes the static loader into an error state after the startup timeout', () => {
		const boot = document.createElement('div');
		boot.id = 'haven-boot';
		boot.setAttribute('aria-busy', 'true');
		boot.innerHTML =
			'<div class="boot-loading-bars"></div><section id="haven-boot-error" hidden></section>';
		document.body.append(boot);
		const recoveryScript = [...appHtmlSource.matchAll(/<script>([\s\S]*?)<\/script>/g)]
			.map((match) => match[1])
			.find((script) => script.includes('window.setTimeout'));
		expect(recoveryScript).toBeTruthy();

		vi.useFakeTimers();
		try {
			new Function(recoveryScript!)();
			vi.advanceTimersByTime(29_999);
			expect(boot.getAttribute('aria-busy')).toBe('true');
			vi.advanceTimersByTime(1);
			expect(boot.getAttribute('aria-busy')).toBe('false');
			expect(boot.getAttribute('aria-live')).toBe('assertive');
			expect(boot.querySelector('.boot-loading-bars')?.hasAttribute('hidden')).toBe(true);
			expect(boot.querySelector('#haven-boot-error')?.hasAttribute('hidden')).toBe(false);
		} finally {
			vi.useRealTimers();
			boot.remove();
		}
	});

	it('supports the compact inline layout for opt-in embedded surfaces', () => {
		render(LoadingState, { variant: 'inline' });

		expect(document.querySelector('.loading-state--inline')).toBeTruthy();
	});
});
