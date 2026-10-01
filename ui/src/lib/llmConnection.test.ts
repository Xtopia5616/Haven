import { describe, expect, it } from 'vitest';
import {
	formatLlmConnectionFailure,
	formatLlmConnectionRecovery,
	llmConnectionReasonText,
	normalizeLlmConnectionReport,
} from './llmConnection.ts';

describe('llm connection report', () => {
	it('keeps typed status and maps a network failure to actionable Chinese text', () => {
		const report = normalizeLlmConnectionReport({
			status: 'disconnected',
			reason: 'network',
			provider: 'PackyAPI',
			model: 'grok-4.6',
		});

		expect(report).toEqual({
			status: 'disconnected',
			reason: 'network',
			provider: 'PackyAPI',
			model: 'grok-4.6',
		});
		expect(formatLlmConnectionFailure(report)).toContain('网络请求失败');
		expect(formatLlmConnectionFailure(report)).toContain('API 地址、API Key 和代理');
	});

	it('falls back safely for malformed backend responses', () => {
		const report = normalizeLlmConnectionReport({ status: 'broken', reason: 'secret' });

		expect(report).toEqual({ status: 'disconnected', reason: 'unknown' });
		expect(llmConnectionReasonText('secret')).toBe('暂时无法确定具体原因');
	});

	it('formats recovery without exposing an endpoint or credential', () => {
		const text = formatLlmConnectionRecovery({
			status: 'ready',
			provider: 'PackyAPI',
			model: 'grok-4.6',
		});

		expect(text).toBe('默认模型（PackyAPI / grok-4.6）已恢复连接');
	});

	it('explains that Continue can retry through an open circuit', () => {
		const report = normalizeLlmConnectionReport({
			status: 'disconnected',
			reason: 'circuit_open',
			provider: 'OpenAI',
			model: 'deepseek-v4-flash',
		});

		expect(formatLlmConnectionFailure(report)).toBe(
			'默认模型（OpenAI / deepseek-v4-flash）处于保护状态，点击“继续生成”可立即重试',
		);
	});
});
