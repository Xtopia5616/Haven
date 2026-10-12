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

		expect(report).toEqual({
			status: 'disconnected',
			reason: 'unknown',
			provider: '',
			model: '',
		});
		expect(llmConnectionReasonText('secret')).toBe('暂时无法确定具体原因');
	});

	it('accepts ready and unconfigured DTOs with required string identity fields', () => {
		expect(
			normalizeLlmConnectionReport({
				status: 'ready',
				provider: 'OpenAI',
				model: 'gpt-5',
			}),
		).toEqual({ status: 'ready', provider: 'OpenAI', model: 'gpt-5' });
		expect(
			normalizeLlmConnectionReport({
				status: 'unconfigured',
				provider: '',
				model: '',
			}),
		).toEqual({ status: 'unconfigured', provider: '', model: '' });
	});

	it('rejects missing or malformed fixed DTO fields and invalid status/reason combinations', () => {
		const invalidReports: unknown[] = [
			{},
			{ status: 'ready', provider: 'OpenAI' },
			{ status: 'ready', provider: 'OpenAI', model: null },
			{ status: 'ready', provider: 1, model: 'gpt-5' },
			{ status: 'ready', reason: 'network', provider: 'OpenAI', model: 'gpt-5' },
			{ status: 'unconfigured', reason: null, provider: '', model: '' },
			{ status: 'disconnected', provider: 'OpenAI', model: 'gpt-5' },
			{ status: 'disconnected', reason: null, provider: 'OpenAI', model: 'gpt-5' },
			{ status: 'disconnected', reason: 'not-a-reason', provider: 'OpenAI', model: 'gpt-5' },
		];

		for (const value of invalidReports) {
			expect(normalizeLlmConnectionReport(value)).toEqual({
				status: 'disconnected',
				reason: 'unknown',
				provider: '',
				model: '',
			});
		}
	});

	it('rejects inherited object-property names as failure reasons', () => {
		for (const reason of ['constructor', 'toString', '__proto__']) {
			expect(llmConnectionReasonText(reason)).toBe('暂时无法确定具体原因');
			expect(
				normalizeLlmConnectionReport({
					status: 'disconnected',
					reason,
					provider: 'OpenAI',
					model: 'gpt-5',
				}),
			).toEqual({
				status: 'disconnected',
				reason: 'unknown',
				provider: '',
				model: '',
			});
		}
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
