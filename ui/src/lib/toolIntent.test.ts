import { describe, expect, it } from 'vitest';
import {
	actionIntentLabel,
	hasToolPreambleBefore,
	hasToolPreambleInBlock,
	TOOL_INTENT_FALLBACK,
} from './toolIntent.ts';

describe('tool intent policy', () => {
	it('uses the first non-empty action description and falls back when absent', () => {
		expect(actionIntentLabel({ title: '整理下载目录' })).toBe('整理下载目录');
		expect(actionIntentLabel({ title: '  ', body: '安装依赖' })).toBe('安装依赖');
		expect(actionIntentLabel({})).toBe(TOOL_INTENT_FALLBACK);
	});

	it('does not treat reasoning as a visible tool preamble', () => {
		const messages = [
			{ role: 'user', content: '查天气' },
			{ role: 'assistant', type: 'reasoning', content: '内部推理' },
			{ role: 'assistant', type: 'tool', content: '' },
		];
		expect(hasToolPreambleBefore(messages, 2)).toBe(false);
	});

	it('recognizes ordinary assistant text immediately before a tool card', () => {
		const messages = [
			{ role: 'user', content: '查天气' },
			{ role: 'assistant', content: '我先查询当前天气。' },
			{ role: 'assistant', type: 'tool', content: '' },
		];
		expect(hasToolPreambleBefore(messages, 2)).toBe(true);
	});

	it('reuses one preamble for every tool in the same live batch', () => {
		const messages = [
			{ id: 'user-1', role: 'user', content: '整理文件并运行测试' },
			{ id: 'step-thought', role: 'assistant', content: '我先整理文件。' },
			{ id: 'step-tool-1', role: 'assistant', type: 'tool', content: '' },
			{ id: 'step-tool-2', role: 'assistant', type: 'tool', content: '' },
		];
		expect(hasToolPreambleInBlock(messages, 'step-thought')).toBe(true);
		expect(hasToolPreambleInBlock(messages, 'step-empty')).toBe(false);
	});
});
