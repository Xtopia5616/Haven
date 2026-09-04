import type {
	AgentCompactionPayload,
	AgentUsagePayload,
} from './contracts/agent.ts';
import type { TauriEvent } from './contracts/session.ts';
import { addNotification } from './stores.ts';
import {
	appendSessionLlmUsage,
	coalesceTokenTotal,
	formatTokenCount,
	updateSessionTokenStats,
} from './sessionUsage.ts';

type UsageEvent = TauriEvent<AgentUsagePayload>;
type CompactionEvent = TauriEvent<AgentCompactionPayload>;

/**
 * Build the usage-related Agent event handlers used by the chat route. Usage
 * state and its presentation helpers stay behind the stores boundary; the
 * route only registers the returned handlers with the IPC event adapter.
 */
export function createChatUsageEventHandlers(): {
	'agent:usage': (event: UsageEvent) => void;
	'agent:compaction': (event: CompactionEvent) => void;
} {
	return {
		'agent:usage': (event) => {
			const d = event.payload;
			if (!d.sessionId) return;
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
			const cumPrompt = d.cumulativePromptTokens || 0;
			const cumCompletion = d.cumulativeCompletionTokens || 0;
			const cumCached = d.cumulativeCachedTokens || 0;
			const cumCreation = d.cumulativeCacheCreationTokens || 0;
			const cumMiss = d.cumulativeCacheMissTokens || 0;
			updateSessionTokenStats(d.sessionId, {
				promptTokens: prompt,
				completionTokens: completion,
				totalTokens: total,
				cachedTokens: cached,
				cacheCreationTokens: creation,
				cacheMissTokens: miss,
				cacheAccounting: d.cacheAccounting || 'unknown',
				contextTokens: d.contextTokens || 0,
				cacheExclusive: !!d.cacheExclusive,
				cumulativePromptTokens: cumPrompt,
				cumulativeCompletionTokens: cumCompletion,
				cumulativeTotalTokens: coalesceTokenTotal(
					cumPrompt,
					cumCompletion,
					d.cumulativeTotalTokens || 0,
					cumCached,
					cumCreation,
				),
				cumulativeCachedTokens: cumCached,
				cumulativeCacheCreationTokens: cumCreation,
				cumulativeCacheMissTokens: cumMiss,
				costUsd: d.costUsd ?? null,
				cumulativeCostUsd: d.cumulativeCostUsd ?? null,
				contextWindow: d.contextWindow ?? null,
				model: d.model ?? null,
				// A real usage event supersedes any restored estimate.
				estimated: false,
				// A live event means the conversation is active again: the widget
				// switches back to the per-step context view.
				restored: false,
			});
			// Also append the per-call detail so tool-card token chips update live
			// — previously they only appeared after restore on resume/reopen.
			if (d.stepNumber != null) {
				appendSessionLlmUsage(d.sessionId, {
					step_number: d.stepNumber,
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
					cost_usd: d.costUsd ?? null,
					has_cost: !!d.hasCost,
					duration_ms: d.durationMs ?? null,
				});
			}
		},
		'agent:compaction': (event) => {
			const d = event.payload;
			const before = formatTokenCount(d.tokensBefore || 0);
			const after = formatTokenCount(d.tokensAfter || 0);
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
	};
}
