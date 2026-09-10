import type {
	AgentCompactionPayload,
	AgentMediaPlanPayload,
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
type MediaPlanEvent = TauriEvent<AgentMediaPlanPayload>;

/**
 * Build the usage-related Agent event handlers used by the chat route. Usage
 * state and its presentation helpers stay behind the stores boundary; the
 * route only registers the returned handlers with the IPC event adapter.
 */
export function createChatUsageEventHandlers(): {
	'agent:usage': (event: UsageEvent) => void;
	'agent:compaction': (event: CompactionEvent) => void;
	'agent:media_plan': (event: MediaPlanEvent) => void;
} {
	return {
		'agent:usage': (event) => {
			const d = event.payload;
			if (!d.sessionId) return;
			const callKind = d.callKind || 'agent';
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
			if (callKind === 'media') {
				if (d.stepNumber != null) {
					appendSessionLlmUsage(d.sessionId, {
						step_number: d.stepNumber,
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
					});
				}
				return;
			}
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
		'agent:media_plan': (event: MediaPlanEvent) => {
			const d = event.payload;
			const reasons = d.notices
				.map((notice) => mediaPlanNoticeLabel(notice.code))
				.filter((label, index, all) => all.indexOf(label) === index);
			if (reasons.length > 0) {
				addNotification(
					`已按 ${d.role} 模型能力调整附件：${reasons.join('、')}`,
					'warning',
					4500,
				);
			}
		},
	};
}

function mediaPlanNoticeLabel(code: string): string {
	const labels: Record<string, string> = {
		raw_capability_unsupported: '模型不支持该媒体类型',
		raw_capability_unknown: '模型能力未知，已安全降级',
		raw_mime_unsupported: '媒体格式不受支持',
		raw_size_exceeded: '媒体超过模型输入上限',
		input_part_limit: '媒体数量超过模型上限',
		no_compatible_representation: '没有兼容的附件表示',
		strategy_excluded: '当前媒体策略已排除原始附件',
		derived_fallback: '已改用派生内容',
		managed_reference_fallback: '已改用受管附件引用',
	};
	return labels[code] || '附件表示已调整';
}
