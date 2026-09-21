import { render, screen } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import SessionTerminationBanner from './SessionTerminationBanner.svelte';

describe('SessionTerminationBanner', () => {
	it('shows the error reason and explains that the conversation is retained', () => {
		render(SessionTerminationBanner, { status: 'error', reason: '响应头等待超过 60 秒' });

		expect(screen.getByRole('alert')).toBeTruthy();
		expect(screen.getByText('错误')).toBeTruthy();
		expect(screen.getByText('响应头等待超过 60 秒')).toBeTruthy();
		expect(screen.getByText(/内容已保留/)).toBeTruthy();
	});

	it('shows the reason for a normally completed conversation', () => {
		render(SessionTerminationBanner, { status: 'completed', reason: '用户主动结束会话' });

		expect(screen.getByRole('status')).toBeTruthy();
		expect(screen.getByText('已结束')).toBeTruthy();
		expect(screen.getByText('用户主动结束会话')).toBeTruthy();
	});

	it('shows the reason when the user interrupts a conversation', () => {
		render(SessionTerminationBanner, { status: 'paused', reason: '用户主动打断输出' });

		expect(screen.getByRole('status')).toBeTruthy();
		expect(screen.getByText('已暂停')).toBeTruthy();
		expect(screen.getByText('用户主动打断输出')).toBeTruthy();
	});

	it('uses a friendly fallback when no reason is available', () => {
		render(SessionTerminationBanner, { status: 'error', reason: '' });

		expect(screen.getByText(/暂未收到更具体的原因/)).toBeTruthy();
	});
});
