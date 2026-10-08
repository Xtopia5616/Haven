import { describe, expect, it } from 'vitest';
import {
	toolRunIntentLabel,
	hasToolPreambleBefore,
	hasToolPreambleInBlock,
	TOOL_INTENT_FALLBACK,
} from './toolIntent.ts';

describe('tool intent policy', () => {
	it('uses the first non-empty action description and falls back when absent', () => {
		expect(toolRunIntentLabel({ title: '整理下载目录' })).toBe('整理下载目录');
		expect(toolRunIntentLabel({ title: '  ', body: '安装依赖' })).toBe('安装依赖');
		expect(toolRunIntentLabel({})).toBe(TOOL_INTENT_FALLBACK);
	});

	it('does not treat reasoning as a visible tool preamble', () => {
		const messages = [
			{ role: 'user' as const, content: '查天气' },
			{ role: 'assistant' as const, type: 'reasoning' as const, content: '内部推理' },
			{ role: 'assistant' as const, type: 'tool' as const, content: '' },
		];
		expect(hasToolPreambleBefore(messages, 2)).toBe(false);
	});

	it('recognizes ordinary assistant text immediately before a tool card', () => {
		const messages = [
			{ role: 'user' as const, content: '查天气' },
			{ role: 'assistant' as const, content: '我先查询当前天气。' },
			{ role: 'assistant' as const, type: 'tool' as const, content: '' },
		];
		expect(hasToolPreambleBefore(messages, 2)).toBe(true);
	});

	it('reuses one preamble for every tool in the same live batch', () => {
		const messages = [
			{ id: 'user-1', role: 'user' as const, content: '整理文件并运行测试' },
			{ id: 'step-thought', role: 'assistant' as const, content: '我先整理文件。' },
			{ id: 'step-tool-1', role: 'assistant' as const, type: 'tool' as const, content: '' },
			{ id: 'step-tool-2', role: 'assistant' as const, type: 'tool' as const, content: '' },
		];
		expect(hasToolPreambleInBlock(messages, 'step-thought')).toBe(true);
		expect(hasToolPreambleInBlock(messages, 'step-empty')).toBe(false);
	});
});
