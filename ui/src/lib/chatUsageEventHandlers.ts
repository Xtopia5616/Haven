import type { AgentMediaPlanPayload, AgentUsagePayload } from './contracts/agent.ts';
import type { TauriEvent } from './contracts/tauriEvent.ts';
import { coalesceTokenTotal } from './sessionUsage.ts';
import type { SessionAction, SessionTokenStats } from './sessionReducer.ts';
import { rememberMediaPlan } from './mediaPlanStore.ts';
import {
	mediaPlanNoticeLabel,
	mediaPlanProjectionLabel,
	mediaPlanStrategyLabel,
} from './mediaPlanPresentation.ts';

type UsageEvent = TauriEvent<AgentUsagePayload>;
type MediaPlanEvent = TauriEvent<AgentMediaPlanPayload>;

/**
 * Build usage and media-plan state handlers used by the chat route. Usage
 * state is reduced through SessionAction; event-driven toast stays in +layout.
 */
export function createChatUsageEventHandlers({
	dispatchSession,
}: {
	dispatchSession: (action: SessionAction) => void;
}): {
	'agent:usage': (event: UsageEvent) => void;
	'agent:media_plan': (event: MediaPlanEvent) => void;
} {
	return {
		'agent:usage': (event) => {
			const d = event.payload;
			if (!d.sessionId) return;
			const callKind = d.callKind;
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
				request_kind: d.requestKind,
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
							cacheDiagnostics: d.cacheDiagnostics,
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
		'agent:media_plan': (event: MediaPlanEvent) => {
			rememberMediaPlan(event.payload);
		},
	};
}
