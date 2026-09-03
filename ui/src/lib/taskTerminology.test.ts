import { describe, expect, it } from 'vitest';
import {
	actionStatusLabel,
	scheduleModeLabel,
	taskKindLabel,
	taskTitle,
} from './taskTerminology.ts';

describe('task terminology', () => {
	it('keeps session and action kinds distinct in the UI', () => {
		expect(taskKindLabel('foreground')).toBe('会话');
		expect(taskKindLabel('background')).toBe('后台任务');
		expect(taskKindLabel('scheduled')).toBe('定时任务');
		expect(taskKindLabel('unknown')).toBe('任务');
	});

	it('maps runtime statuses and scheduled modes to Chinese labels', () => {
		expect(actionStatusLabel('completed')).toBe('已完成');
		expect(actionStatusLabel('not_found')).toBe('未找到');
		expect(scheduleModeLabel('tool')).toBe('调用工具');
		expect(scheduleModeLabel('continue')).toBe('继续会话');
		expect(scheduleModeLabel(undefined)).toBe('调用工具');
	});

	it('does not turn an internal command into a task title', () => {
		expect(taskTitle({ kind: 'background', title: '  整理下载目录  ', body: 'body' })).toBe(
			'整理下载目录',
		);
		expect(taskTitle({ kind: 'background', command: 'dir' })).toBe('调用工具');
		expect(taskTitle({ kind: 'scheduled' })).toBe('定时任务');
	});
});
