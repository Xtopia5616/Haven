import { render, screen } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import SessionRunEndBanner from './SessionRunEndBanner.svelte';

describe('SessionRunEndBanner', () => {
	it('shows the error reason and distinguishes saved content from the failed part', () => {
		render(SessionRunEndBanner, { status: 'error', reason: '响应头等待超过 60 秒' });

		expect(screen.getByRole('alert')).toBeTruthy();
		expect(screen.getByText('错误')).toBeTruthy();
		expect(screen.getByText('响应头等待超过 60 秒')).toBeTruthy();
		expect(screen.getByText(/已提交的内容仍可恢复/)).toBeTruthy();
		expect(screen.getByText(/失败的这一步可以使用下方“继续生成”重试/)).toBeTruthy();
	});

	it('shows the reason for a normally completed conversation', () => {
		render(SessionRunEndBanner, { status: 'completed', reason: '用户主动结束会话' });

		expect(screen.getByRole('status')).toBeTruthy();
		expect(screen.getByText('已结束')).toBeTruthy();
		expect(screen.getByText('用户主动结束会话')).toBeTruthy();
	});

	it('shows the reason when the user interrupts a conversation', () => {
		render(SessionRunEndBanner, { status: 'paused', reason: '用户主动打断输出' });

		expect(screen.getByRole('status')).toBeTruthy();
		expect(screen.getByText('已暂停')).toBeTruthy();
		expect(screen.getByText('用户主动打断输出')).toBeTruthy();
	});

	it('uses a friendly fallback when no reason is available', () => {
		render(SessionRunEndBanner, { status: 'error', reason: '' });

		expect(screen.getByText(/暂未收到更具体的原因/)).toBeTruthy();
	});
});
