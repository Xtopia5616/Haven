import {
	coalesceTokenTotal,
	cumulativeCacheHitRatePercent,
	formatCostUsd,
	formatTokenCount,
	type LlmUsage,
} from './sessionUsage';

export interface SessionTokenStats {
	promptTokens?: number;
	completionTokens?: number;
	totalTokens?: number;
	cachedTokens?: number;
	cacheCreationTokens?: number;
	cacheMissTokens?: number;
	cacheAccounting?: string;
	contextTokens?: number;
	cacheExclusive?: boolean;
	cumulativePromptTokens?: number;
	cumulativeCompletionTokens?: number;
	cumulativeTotalTokens?: number;
	cumulativeCachedTokens?: number;
	cumulativeCacheCreationTokens?: number;
	cumulativeCacheMissTokens?: number;
	cumulativeCostUsd?: number | null;
	contextWindow?: number | null;
	model?: string | null;
	estimated?: boolean;
	restored?: boolean;
}

export interface TokenUsageDetails {
	currentPromptTokens: number;
	currentCompletionTokens: number;
	currentTotalTokens: number;
	currentCachedTokens: number;
	currentCacheCreationTokens: number;
	currentCacheMissTokens: number;
	currentCacheRatePercent: number | null;
	contextTokens: number;
	contextWindow: number | null;
	contextRatePercent: number | null;
	cumulativePromptTokens: number;
	cumulativeCompletionTokens: number;
	cumulativeTotalTokens: number;
	cumulativeCachedTokens: number;
	cumulativeCacheCreationTokens: number;
	cumulativeCacheMissTokens: number;
	cumulativeCacheRatePercent: number | null;
	callCount: number;
	model: string | null;
	costUsd: number | null;
}

export interface ToolDataUsage {
	args: number;
	result: number;
	total: number;
}

/**
 * Estimate the token footprint of one tool card's own data. Provider usage is
 * reported for the whole model response, so it cannot be split precisely
 * between parallel tool calls; this estimate keeps the per-tool view useful
 * without presenting it as billable provider usage.
 */
export function estimateToolDataTokens(
	toolName: string,
	toolArgs: unknown,
	toolResult: unknown,
): ToolDataUsage | null {
	const argsText = stringifyForTokenEstimate(toolArgs);
	const inputText = [toolName, argsText].filter(Boolean).join('\n');
	const resultText = stringifyForTokenEstimate(toolResult);
	const args = estimateTextTokens(inputText);
	const result = estimateTextTokens(resultText);
	if (args === 0 && result === 0) return null;
	return { args, result, total: args + result };
}

function stringifyForTokenEstimate(value: unknown): string {
	if (value == null || value === '') return '';
	if (typeof value === 'string') return value;
	try {
		return JSON.stringify(value) ?? '';
	} catch {
		return String(value);
	}
}

/**
 * Browser-side approximation: CJK characters are counted individually and
 * other text is grouped at roughly four characters per token. The exact
 * provider tokenizer is model-specific and intentionally stays out of this
 * presentation-only estimate.
 */
function estimateTextTokens(value: string): number {
	if (!value) return 0;
	let cjk = 0;
	let other = 0;
	for (const character of value) {
		if (
			/[\u3040-\u30ff\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff\uac00-\ud7af]/u.test(character)
		) {
			cjk += 1;
		} else {
			other += 1;
		}
	}
	return cjk + (other > 0 ? Math.ceil(other / 4) : 0);
}

/** Calculate a cache-hit percentage for one live usage snapshot. */
function cacheHitRatePercent(
	prompt: number,
	cached: number,
	creation = 0,
	exclusive = false,
): number | null {
	if (!cached || cached <= 0) return null;
	const denominator = exclusive ? (prompt || 0) + cached + (creation || 0) : prompt || 0;
	if (!denominator) return null;
	return Math.min(100, (cached / denominator) * 100);
}

/**
 * Project raw session usage into the fields needed by the detail popover.
 * Restored sessions use the last persisted call for "current" values because
 * the live event stream is no longer present after reopening a conversation.
 */
export function buildTokenUsageDetails(
	stats: SessionTokenStats,
	llmUsage: LlmUsage[],
): TokenUsageDetails {
	const lastCall = llmUsage.at(-1);
	const useLastCall = !!stats.restored && !!lastCall;
	const currentPromptTokens = useLastCall
		? lastCall?.prompt_tokens || 0
		: stats.promptTokens || 0;
	const currentCompletionTokens = useLastCall
		? lastCall?.completion_tokens || 0
		: stats.completionTokens || 0;
	const currentCachedTokens = useLastCall
		? lastCall?.cached_tokens || 0
		: stats.cachedTokens || 0;
	const currentCacheCreationTokens = useLastCall
		? lastCall?.cache_creation_tokens || 0
		: stats.cacheCreationTokens || 0;
	const currentCacheMissTokens = useLastCall
		? lastCall?.cache_miss_tokens || 0
		: stats.cacheMissTokens || 0;
	const currentAccounting = useLastCall
		? lastCall?.cache_accounting || 'unknown'
		: stats.cacheAccounting || (stats.cacheExclusive ? 'exclusive' : 'unknown');
	const currentTotalTokens = coalesceTokenTotal(
		currentPromptTokens,
		currentCompletionTokens,
		useLastCall ? lastCall?.total_tokens || 0 : stats.totalTokens || 0,
		currentCachedTokens,
		currentCacheCreationTokens,
		currentAccounting,
	);
	const contextTokens = stats.contextTokens || lastCall?.context_tokens || currentPromptTokens;
	const contextWindow = stats.contextWindow || lastCall?.context_window || null;
	const contextRatePercent =
		contextWindow && contextTokens > 0
			? Math.min(100, (contextTokens / contextWindow) * 100)
			: null;
	const cumulativePromptTokens = stats.cumulativePromptTokens || 0;
	const cumulativeCompletionTokens = stats.cumulativeCompletionTokens || 0;
	const cumulativeCachedTokens = stats.cumulativeCachedTokens || 0;
	const cumulativeCacheCreationTokens = stats.cumulativeCacheCreationTokens || 0;
	const cumulativeCacheMissTokens = stats.cumulativeCacheMissTokens || 0;

	return {
		currentPromptTokens,
		currentCompletionTokens,
		currentTotalTokens,
		currentCachedTokens,
		currentCacheCreationTokens,
		currentCacheMissTokens,
		currentCacheRatePercent: ['inclusive', 'exclusive'].includes(currentAccounting)
			? cacheHitRatePercent(
					currentPromptTokens,
					currentCachedTokens,
					currentCacheCreationTokens,
					currentAccounting === 'exclusive',
				)
			: null,
		contextTokens,
		contextWindow,
		contextRatePercent,
		cumulativePromptTokens,
		cumulativeCompletionTokens,
		cumulativeTotalTokens: coalesceTokenTotal(
			cumulativePromptTokens,
			cumulativeCompletionTokens,
			stats.cumulativeTotalTokens || 0,
			cumulativeCachedTokens,
			cumulativeCacheCreationTokens,
		),
		cumulativeCachedTokens,
		cumulativeCacheCreationTokens,
		cumulativeCacheMissTokens,
		cumulativeCacheRatePercent: cumulativeCacheHitRatePercent(llmUsage),
		callCount: llmUsage.length || (stats.totalTokens ? 1 : 0),
		model: stats.model || lastCall?.model || null,
		costUsd: stats.cumulativeCostUsd ?? null,
	};
}

/** Build the tooltip for the chat token usage widget. */
export function buildTokenUsageTooltip(stats: SessionTokenStats, llmUsage: LlmUsage[]): string {
	const details = buildTokenUsageDetails(stats, llmUsage);
	const parts: string[] = [];
	const cumulativePrompt = details.cumulativePromptTokens;
	const cumulativeCompletion = details.cumulativeCompletionTokens;
	const cumulativeCached = details.cumulativeCachedTokens;
	const cumulativeCreation = details.cumulativeCacheCreationTokens;
	const cumulativeTotal = details.cumulativeTotalTokens;
	if (stats.restored) {
		parts.push(`累计上传 ${cumulativePrompt} → 累计生成 ${cumulativeCompletion} tokens`);
		parts.push(`累计 ${cumulativeTotal} tokens`);
	} else {
		parts.push(`上传 ${stats.promptTokens || 0} → 生成 ${stats.completionTokens || 0} tokens`);
		parts.push(`累计 ${cumulativeTotal} tokens`);
		if (stats.cumulativePromptTokens != null) {
			parts.push(
				`累计上传 ${stats.cumulativePromptTokens} → 累计生成 ${stats.cumulativeCompletionTokens} tokens`,
			);
		}
	}
	const liveCached = details.currentCachedTokens;
	const liveCreation = details.currentCacheCreationTokens;
	const liveMiss = details.currentCacheMissTokens;
	if (!stats.restored && (liveCached > 0 || liveCreation > 0)) {
		const rate = details.currentCacheRatePercent;
		let line = `本次缓存命中 ${formatTokenCount(liveCached)}`;
		if (rate != null) line += `（${rate.toFixed(0)}%）`;
		if (liveCreation > 0) line += ` / 写入 ${formatTokenCount(liveCreation)}`;
		if (liveMiss > 0) line += ` / 未命中 ${formatTokenCount(liveMiss)}`;
		parts.push(line);
	}
	if (cumulativeCached > 0 || cumulativeCreation > 0) {
		const rate = cumulativeCacheHitRatePercent(llmUsage);
		let line = `累计缓存命中 ${formatTokenCount(cumulativeCached)}`;
		if (rate != null) line += `（${rate.toFixed(0)}%）`;
		if (cumulativeCreation > 0) line += ` / 写入 ${formatTokenCount(cumulativeCreation)}`;
		parts.push(line);
	}
	if (llmUsage.length > 0) parts.push(`调用 ${llmUsage.length} 次`);
	if (details.model) parts.push(`模型 ${details.model}`);
	if (details.contextWindow) {
		const percentage =
			details.contextRatePercent != null ? `${details.contextRatePercent.toFixed(0)}%` : '?';
		parts.push(`上下文 ${percentage} / ${formatTokenCount(details.contextWindow)}`);
	}
	if (details.costUsd != null) parts.push(`费用 ${formatCostUsd(details.costUsd)}`);
	if (stats.estimated) parts.push('估算值（历史对话，未计费）');
	return parts.join('\n');
}
