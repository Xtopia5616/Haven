import { describe, expect, it } from 'vitest';
import {
	toolRunStatusLabel,
	scheduleModeLabel,
	toolRunKindLabel,
	toolRunTitle,
} from './toolRunTerminology.ts';

describe('task terminology', () => {
	it('keeps session and ToolRun kinds distinct in the UI', () => {
		expect(toolRunKindLabel('foreground')).toBe('会话');
		expect(toolRunKindLabel('background')).toBe('后台任务');
		expect(toolRunKindLabel('scheduled')).toBe('定时任务');
		expect(toolRunKindLabel('unknown')).toBe('任务');
	});

	it('maps runtime statuses and scheduled modes to Chinese labels', () => {
		expect(toolRunStatusLabel('completed')).toBe('已完成');
		expect(toolRunStatusLabel('not_found')).toBe('未找到');
		expect(scheduleModeLabel('tool')).toBe('调用工具');
		expect(scheduleModeLabel('continue')).toBe('继续会话');
		expect(scheduleModeLabel(undefined)).toBe('调用工具');
	});

	it('does not turn an internal command into a task title', () => {
		expect(toolRunTitle({ kind: 'background', title: '  整理下载目录  ', body: 'body' })).toBe(
			'整理下载目录',
		);
		expect(toolRunTitle({ kind: 'background', command: 'dir' })).toBe('调用工具');
		expect(toolRunTitle({ kind: 'scheduled' })).toBe('定时任务');
	});
});
