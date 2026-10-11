import { describe, expect, it } from 'vitest';
import { formatAgentCompactionToast, formatAgentMediaPlanToast } from './agentEventToast.ts';

describe('Agent event toast projection', () => {
	it('formats normal and degraded compaction events', () => {
		expect(
			formatAgentCompactionToast({
				sessionId: 'ses-test',
				summary: 'summary',
				tokensBefore: 1200,
				tokensAfter: 800,
				degraded: false,
			}),
		).toEqual({ message: '上下文压缩：1.2K → 800 tokens', type: 'info', durationMs: 2500 });

		expect(
			formatAgentCompactionToast({
				sessionId: 'ses-test',
				summary: 'summary',
				tokensBefore: 1200,
				tokensAfter: 800,
				degraded: true,
			}),
		).toEqual({
			message: '上下文空间不足，已降级压缩：1.2K → 800 tokens；较早内容已省略',
			type: 'warning',
			durationMs: 4000,
		});
	});

	it('includes media representation and downgrade reason in the toast', () => {
		expect(
			formatAgentMediaPlanToast({
				sessionId: 'ses-media',
				stepNumber: 4,
				runId: 8,
				role: 'audio_chat',
				strategy: 'auto',
				projections: [
					{ assetId: 'asset-1', representation: 'transcript', mode: 'derived' },
				],
				notices: [{ assetId: 'asset-1', code: 'raw_capability_unknown' }],
			}),
		).toEqual({
			message:
				'附件表示（audio_chat）：已使用 asset-1：转写文本（派生）；策略：自动；原因：当前模型能力未知，未发送原始媒体',
			type: 'warning',
			durationMs: 5000,
		});
	});
});
