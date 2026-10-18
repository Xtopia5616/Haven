import { describe, expect, it } from 'vitest';
import {
	toolRunStatusLabel,
	scheduleModeLabel,
	toolRunKindLabel,
	toolRunTitle,
} from './toolRunTerminology.ts';

describe('ToolRun terminology', () => {
	it('labels the only ToolRun kind in the shared contract', () => {
		expect(toolRunKindLabel('background')).toBe('后台任务');
		expect(toolRunKindLabel(undefined)).toBe('后台任务');
	});

	it('maps runtime statuses and scheduled modes to Chinese labels', () => {
		expect(toolRunStatusLabel('completed')).toBe('已完成');
		expect(toolRunStatusLabel('not_found')).toBe('未找到');
		expect(scheduleModeLabel('tool')).toBe('调用工具');
		expect(scheduleModeLabel('continue')).toBe('继续会话');
		expect(scheduleModeLabel(undefined)).toBe('调用工具');
	});

	it('uses the target tool name as the task title', () => {
		expect(toolRunTitle({ toolName: 'files.write' })).toBe('后台调用 · files.write');
		expect(toolRunTitle({})).toBe('后台工具调用');
	});
});
