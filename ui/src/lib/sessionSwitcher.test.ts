import { describe, expect, it } from 'vitest';
import { buildSessionSwitcherOptions } from './sessionSwitcher.ts';
import type { SessionHistoryRow } from './contracts/sessionHistory.ts';
import type { SessionSummary } from './sessionReducer/types.ts';

describe('buildSessionSwitcherOptions', () => {
	it('keeps completed history switchable and prefers live summaries for duplicates', () => {
		const live: SessionSummary[] = [
			{ id: 'ses-current', status: 'running', title: '正在绘画' },
		];
		const history = [
			{
				id: 'ses-old',
				input_text: '旧绘画会话',
				title: null,
				status: 'completed',
			},
			{
				id: 'ses-current',
				input_text: '已完成的旧快照',
				title: null,
				status: 'completed',
			},
		] as SessionHistoryRow[];

		const options = buildSessionSwitcherOptions(live, history);

		expect(options.map((session) => session.id)).toEqual(['ses-current', 'ses-old']);
		expect(options[0]).toMatchObject({ id: 'ses-current', status: 'running' });
		expect(options[1]).toMatchObject({
			id: 'ses-old',
			status: 'completed',
			title: '旧绘画会话',
		});
	});
});
