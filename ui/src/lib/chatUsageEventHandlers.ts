import type {
	AgentCompactionPayload,
	AgentMediaPlanPayload,
	AgentUsagePayload,
} from './contracts/agent.ts';
import type { TauriEvent } from './contracts/session.ts';
import { addNotification } from './stores.ts';
import { coalesceTokenTotal, formatTokenCount } from './sessionUsage.ts';
import type { SessionAction, SessionTokenStats } from './sessionReducer.ts';
import { rememberMediaPlan } from './stores.ts';
import logger from '$lib/logger.ts';
import {
	mediaPlanNoticeLabel,
	mediaPlanProjectionLabel,
	mediaPlanStrategyLabel,
} from './mediaPlanPresentation.ts';

type UsageEvent = TauriEvent<AgentUsagePayload>;
type CompactionEvent = TauriEvent<AgentCompactionPayload>;
type MediaPlanEvent = TauriEvent<AgentMediaPlanPayload>;

/**
 * Build the usage-related Agent event handlers used by the chat route. Usage
 * state is reduced through SessionAction.
 */
export function createChatUsageEventHandlers({
	dispatchSession,
}: {
	dispatchSession: (action: SessionAction) => void;
}): {
	'agent:usage': (event: UsageEvent) => void;
	'agent:compaction': (event: CompactionEvent) => void;
	'agent:media_plan': (event: MediaPlanEvent) => void;
} {
	return {
		'agent:usage': (event) => {
			const d = event.payload;
			if (!d.sessionId) return;
			const callKind = d.callKind;
			if (callKind !== 'agent' && callKind !== 'media' && callKind !== 'tool') {
				logger.error(
					'chatUsageEventHandlers',
					`Unsupported usage call kind: ${String(callKind)}`,
				);
				return;
			}
			const prompt = d.promptTokens || 0;
			const completion = d.completionTokens || 0;
			const cached = d.cachedTokens || 0;
			const creation = d.cacheCreationTokens || 0;
			const miss = d.cacheMissTokens || 0;
			const total = coalesceTokenTotal(
				prompt,
				completion,
				d.totalTokens || 0,
				cached,
				creation,
				d.cacheAccounting || 'unknown',
			);
			const call = {
				step_number: d.stepNumber ?? null,
				call_kind: callKind,
				role: d.role || undefined,
				model: d.model ?? null,
				prompt_tokens: prompt,
				completion_tokens: completion,
				total_tokens: total,
				cached_tokens: cached,
				cache_creation_tokens: creation,
				cache_miss_tokens: miss,
				cache_accounting: d.cacheAccounting || 'unknown',
				cache_diagnostics: d.cacheDiagnostics || undefined,
				context_tokens: d.contextTokens || 0,
				context_window: d.contextWindow ?? null,
				cost_usd: d.costUsd ?? null,
				has_cost: !!d.hasCost,
				duration_ms: d.durationMs ?? null,
			};
			const stats: SessionTokenStats | undefined =
				callKind === 'agent'
					? {
							promptTokens: prompt,
							completionTokens: completion,
							totalTokens: total,
							cachedTokens: cached,
							cacheCreationTokens: creation,
							cacheMissTokens: miss,
							cacheAccounting: d.cacheAccounting || 'unknown',
							contextTokens: d.contextTokens || 0,
							cacheExclusive: !!d.cacheExclusive,
							cumulativePromptTokens: d.cumulativePromptTokens || 0,
							cumulativeCompletionTokens: d.cumulativeCompletionTokens || 0,
							cumulativeTotalTokens: coalesceTokenTotal(
								d.cumulativePromptTokens || 0,
								d.cumulativeCompletionTokens || 0,
								d.cumulativeTotalTokens || 0,
								d.cumulativeCachedTokens || 0,
								d.cumulativeCacheCreationTokens || 0,
							),
							cumulativeCachedTokens: d.cumulativeCachedTokens || 0,
							cumulativeCacheCreationTokens: d.cumulativeCacheCreationTokens || 0,
							cumulativeCacheMissTokens: d.cumulativeCacheMissTokens || 0,
							costUsd: d.costUsd ?? null,
							cumulativeCostUsd: d.cumulativeCostUsd ?? null,
							contextWindow: d.contextWindow ?? null,
							model: d.model ?? null,
						}
					: undefined;
			dispatchSession({
				type: 'session/usage-live',
				sessionId: d.sessionId,
				...(stats ? { stats } : {}),
				...(callKind !== 'agent' || d.stepNumber != null ? { call } : {}),
			});
		},
		'agent:compaction': (event) => {
			const d = event.payload;
			const before = formatTokenCount(d.tokensBefore);
			const after = formatTokenCount(d.tokensAfter);
			if (d.degraded) {
				addNotification(
					`上下文空间不足，已降级压缩：${before} → ${after} tokens；较早内容已省略`,
					'warning',
					4000,
				);
			} else {
				addNotification(`上下文压缩：${before} → ${after} tokens`, 'info', 2500);
			}
		},
		'agent:media_plan': (event: MediaPlanEvent) => {
			const d = event.payload;
			rememberMediaPlan(d);
			const projections = d.projections.map(mediaPlanProjectionLabel);
			const reasons = d.notices
				.map((notice) => mediaPlanNoticeLabel(notice.code))
				.filter((label, index, all) => all.indexOf(label) === index);
			const details = [
				projections.length > 0
					? `已使用 ${projections.join('、')}`
					: '没有发送兼容的附件表示',
				`策略：${mediaPlanStrategyLabel(d.strategy)}`,
				...reasons.map((reason) => `原因：${reason}`),
			];
			if (details.length > 0) {
				addNotification(
					`附件表示（${d.role}）：${details.join('；')}`,
					d.notices.length > 0 ? 'warning' : 'info',
					5000,
				);
			}
		},
	};
}
