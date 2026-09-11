import { render, screen } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import SessionErrorBanner from './SessionErrorBanner.svelte';

describe('SessionErrorBanner', () => {
	it('shows the error reason and explains that the conversation is retained', () => {
		render(SessionErrorBanner, { reason: '响应头等待超过 60 秒' });

		expect(screen.getByRole('alert')).toBeTruthy();
		expect(screen.getByText('错误')).toBeTruthy();
		expect(screen.getByText('响应头等待超过 60 秒')).toBeTruthy();
		expect(screen.getByText(/内容已保留/)).toBeTruthy();
	});

	it('uses a friendly fallback when no reason is available', () => {
		render(SessionErrorBanner, { reason: '' });

		expect(screen.getByText(/暂未收到更具体的原因/)).toBeTruthy();
	});
});
