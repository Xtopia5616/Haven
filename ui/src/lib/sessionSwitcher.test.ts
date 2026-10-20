import { describe, expect, it } from 'vitest';
import { buildSessionSwitcherOptions } from './sessionSwitcher.ts';
import type { SessionHistoryRow } from './contracts/sessionHistory.ts';
import type { SessionSummary } from './sessionReducer/types.ts';

describe('buildSessionSwitcherOptions', () => {
	it('keeps open, paused, and errored sessions while hiding completed history', () => {
		const live: SessionSummary[] = [
			{
				sessionId: 'ses-1bd4a9f01b95da1dfb71021b982a09a8',
				status: 'running',
				title: '正在绘画',
			},
			{
				sessionId: 'ses-440bd6f0b8eb6c499f16b46d66dbd3e8',
				status: 'paused',
				title: '暂停中',
			},
			{
				sessionId: 'ses-a79b423834ab0bb6a2d47517cd70d1b3',
				status: 'error',
				title: '运行中断',
			},
			{
				sessionId: 'ses-37eb2b180f5a8aca7d542148196b823c',
				status: 'completed',
				title: '已完成',
			},
		];
		const history = [
			{
				sessionId: 'ses-76f19db031ba6b93089e8acb9f43666f',
				input_text: '已暂停的历史会话',
				title: null,
				status: 'paused',
			},
			{
				sessionId: 'ses-37eb2b180f5a8aca7d542148196b823c',
				input_text: '已完成但历史快照过期',
				title: null,
				status: 'paused',
			},
			{
				sessionId: 'ses-a999d7a0313c0ea8bc38fbc1069f91ce',
				input_text: '错误中断的历史会话',
				title: null,
				status: 'error',
			},
			{
				sessionId: 'ses-2009855d9579b84ce67e23e67d6ed107',
				input_text: '已完成历史会话',
				title: null,
				status: 'completed',
			},
		] as SessionHistoryRow[];

		const options = buildSessionSwitcherOptions(live, history);

		expect(options.map((session) => session.sessionId)).toEqual([
			'ses-1bd4a9f01b95da1dfb71021b982a09a8',
			'ses-440bd6f0b8eb6c499f16b46d66dbd3e8',
			'ses-a79b423834ab0bb6a2d47517cd70d1b3',
			'ses-76f19db031ba6b93089e8acb9f43666f',
			'ses-a999d7a0313c0ea8bc38fbc1069f91ce',
		]);
		expect(options[0]).toMatchObject({
			sessionId: 'ses-1bd4a9f01b95da1dfb71021b982a09a8',
			status: 'running',
		});
		expect(options[1]).toMatchObject({
			sessionId: 'ses-440bd6f0b8eb6c499f16b46d66dbd3e8',
			status: 'paused',
			title: '暂停中',
		});
		expect(options[3]).toMatchObject({
			sessionId: 'ses-76f19db031ba6b93089e8acb9f43666f',
			status: 'paused',
			title: '已暂停的历史会话',
			inputText: '已暂停的历史会话',
		});
		expect(options[4]).toMatchObject({
			sessionId: 'ses-a999d7a0313c0ea8bc38fbc1069f91ce',
			status: 'error',
		});
		expect(options[3]).not.toHaveProperty('input_text');
		expect(
			options.some((session) => session.sessionId === 'ses-37eb2b180f5a8aca7d542148196b823c'),
		).toBe(false);
		expect(
			options.some((session) => session.sessionId === 'ses-2009855d9579b84ce67e23e67d6ed107'),
		).toBe(false);
	});
});
