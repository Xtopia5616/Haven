import {
	coalesceTokenTotal,
	cumulativeCacheHitRatePercent,
	formatCostUsd,
	formatTokenCount,
} from './sessionUsage';
import type { SessionLlmUsage } from './contracts/sessionHistory.ts';

/** Optional display-facing statistics; reducer state keeps its stricter model type. */
export interface SessionTokenStatsView {
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
	cacheDiagnostics?: unknown;
	restored?: boolean;
}

export interface CacheDiagnosticsSummary {
	mode: string | null;
	provider: string | null;
	outcome: string | null;
	downgraded: boolean;
	usageSource: string | null;
}

export interface TokenUsageDetails {
	currentPromptTokens: number;
	currentCompletionTokens: number;
	currentTotalTokens: number;
	currentCachedTokens: number;
	currentCacheCreationTokens: number;
	currentCacheMissTokens: number;
	currentCacheRatePercent: number | null;
	currentCacheKnown?: boolean;
	currentCacheDiagnostics?: CacheDiagnosticsSummary | null;
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
	cumulativeCacheKnown?: boolean;
	callCount: number;
	mediaCallCount: number;
	mediaTotalTokens: number;
	mediaCostUsd: number | null;
	toolCallCount: number;
	toolTotalTokens: number;
	toolCostUsd: number | null;
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
	const denominator = exclusive ? (prompt || 0) + cached + (creation || 0) : prompt || 0;
	if (!denominator) return null;
	return Math.min(100, Math.max(0, (cached / denominator) * 100));
}

function summarizeCacheDiagnostics(value: unknown): CacheDiagnosticsSummary | null {
	if (value == null || typeof value !== 'object' || Array.isArray(value)) return null;
	const raw = value as Record<string, unknown>;
	return {
		mode: typeof raw.mode === 'string' ? raw.mode : null,
		provider: typeof raw.provider === 'string' && raw.provider ? raw.provider : null,
		outcome: typeof raw.outcome === 'string' ? raw.outcome : null,
		downgraded: raw.downgraded === true,
		usageSource: typeof raw.usage_source === 'string' ? raw.usage_source : null,
	};
}

function cacheUsageIsKnown(diagnostics: CacheDiagnosticsSummary | null): boolean {
	if (!diagnostics) return true;
	if (diagnostics.outcome === 'disabled') return true;
	if (diagnostics.outcome === 'unknown' || diagnostics.usageSource === 'unavailable')
		return false;
	return true;
}

export function cacheModeLabel(mode: string | null | undefined): string {
	switch (mode) {
		case 'off':
			return '已关闭';
		case 'key':
			return '缓存 key';
		case 'split':
			return '分离系统提示词';
		case 'implicit':
			return '自动前缀缓存';
		case 'explicit':
			return '显式缓存';
		default:
			return '未知';
	}
}

export function cacheOutcomeLabel(outcome: string | null | undefined): string {
	switch (outcome) {
		case 'disabled':
			return '未启用';
		case 'hit':
			return '命中';
		case 'miss':
			return '未命中';
		default:
			return '未知';
	}
}

/**
 * Project raw session usage into the fields needed by the detail popover.
 * Restored sessions use the last persisted call for "current" values because
 * the live event stream is no longer present after reopening a session.
 */
export function buildTokenUsageDetails(
	stats: SessionTokenStatsView,
	llmUsage: SessionLlmUsage[],
): TokenUsageDetails {
	const agentCalls = llmUsage.filter((call) => call.call_kind === 'agent');
	const mediaCalls = llmUsage.filter((call) => call.call_kind === 'media');
	const toolCalls = llmUsage.filter((call) => call.call_kind === 'tool');
	const lastCall = agentCalls.at(-1);
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
	const currentCacheDiagnostics = summarizeCacheDiagnostics(
		useLastCall
			? lastCall?.cache_diagnostics
			: (stats.cacheDiagnostics ?? lastCall?.cache_diagnostics),
	);
	const currentCacheKnown = cacheUsageIsKnown(currentCacheDiagnostics);
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
	const cumulativeCacheKnown =
		agentCalls.length > 0
			? agentCalls.every((call) =>
					cacheUsageIsKnown(summarizeCacheDiagnostics(call.cache_diagnostics)),
				)
			: currentCacheKnown;

	return {
		currentPromptTokens,
		currentCompletionTokens,
		currentTotalTokens,
		currentCachedTokens,
		currentCacheCreationTokens,
		currentCacheMissTokens,
		currentCacheRatePercent:
			currentCacheKnown && ['inclusive', 'exclusive'].includes(currentAccounting)
				? cacheHitRatePercent(
						currentPromptTokens,
						currentCachedTokens,
						currentCacheCreationTokens,
						currentAccounting === 'exclusive',
					)
				: null,
		currentCacheKnown,
		currentCacheDiagnostics,
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
		cumulativeCacheRatePercent: cumulativeCacheKnown
			? cumulativeCacheHitRatePercent(llmUsage)
			: null,
		cumulativeCacheKnown,
		callCount: agentCalls.length || (stats.totalTokens ? 1 : 0),
		mediaCallCount: mediaCalls.length,
		mediaTotalTokens: mediaCalls.reduce(
			(total, call) =>
				total +
				coalesceTokenTotal(
					call.prompt_tokens || 0,
					call.completion_tokens || 0,
					call.total_tokens || 0,
					call.cached_tokens || 0,
					call.cache_creation_tokens || 0,
					call.cache_accounting || 'unknown',
				),
			0,
		),
		mediaCostUsd: mediaCalls.some((call) => call.has_cost)
			? mediaCalls.reduce(
					(total, call) => total + (call.has_cost ? call.cost_usd || 0 : 0),
					0,
				)
			: null,
		toolCallCount: toolCalls.length,
		toolTotalTokens: toolCalls.reduce(
			(total, call) =>
				total +
				coalesceTokenTotal(
					call.prompt_tokens || 0,
					call.completion_tokens || 0,
					call.total_tokens || 0,
					call.cached_tokens || 0,
					call.cache_creation_tokens || 0,
					call.cache_accounting || 'unknown',
				),
			0,
		),
		toolCostUsd: toolCalls.some((call) => call.has_cost)
			? toolCalls.reduce((total, call) => total + (call.has_cost ? call.cost_usd || 0 : 0), 0)
			: null,
		model: stats.model || lastCall?.model || null,
		costUsd: stats.cumulativeCostUsd ?? null,
	};
}

/** Build the tooltip for the chat token usage widget. */
export function buildTokenUsageTooltip(
	stats: SessionTokenStatsView,
	llmUsage: SessionLlmUsage[],
): string {
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
	if (details.currentCacheDiagnostics) {
		const diagnostics = details.currentCacheDiagnostics;
		const diagnosticLine = [
			`缓存策略 ${cacheModeLabel(diagnostics.mode)}`,
			`结果 ${cacheOutcomeLabel(diagnostics.outcome)}`,
			diagnostics.provider ? `提供方 ${diagnostics.provider}` : null,
			diagnostics.usageSource === 'provider'
				? '用量来源 provider'
				: diagnostics.usageSource === 'unavailable'
					? '用量来源未提供'
					: '用量来源未知',
			diagnostics.downgraded ? '已降级' : '未降级',
		]
			.filter(Boolean)
			.join(' · ');
		parts.push(diagnosticLine);
	}
	if (!details.currentCacheKnown) {
		parts.push('本次缓存计数未知');
	} else if (!stats.restored && (liveCached > 0 || liveCreation > 0)) {
		const rate = details.currentCacheRatePercent;
		let line = `本次缓存命中 ${formatTokenCount(liveCached)}`;
		if (rate != null) line += `（${rate.toFixed(0)}%）`;
		if (liveCreation > 0) line += ` / 写入 ${formatTokenCount(liveCreation)}`;
		if (liveMiss > 0) line += ` / 未命中 ${formatTokenCount(liveMiss)}`;
		parts.push(line);
	}
	if (!details.cumulativeCacheKnown) {
		parts.push('累计缓存统计未知');
	} else if (cumulativeCached > 0 || cumulativeCreation > 0) {
		const rate = cumulativeCacheHitRatePercent(llmUsage);
		let line = `累计缓存命中 ${formatTokenCount(cumulativeCached)}`;
		if (rate != null) line += `（${rate.toFixed(0)}%）`;
		if (cumulativeCreation > 0) line += ` / 写入 ${formatTokenCount(cumulativeCreation)}`;
		parts.push(line);
	}
	if (details.callCount > 0) parts.push(`调用 ${details.callCount} 次`);
	if (details.mediaCallCount > 0) {
		let line =
			'媒体推理 ' +
			details.mediaCallCount +
			' 次 / ' +
			formatTokenCount(details.mediaTotalTokens) +
			' tokens';
		if (details.mediaCostUsd != null) {
			line += ' / 费用 ' + (formatCostUsd(details.mediaCostUsd) || '');
		}
		parts.push(line);
	}
	if (details.toolCallCount > 0) {
		let line =
			'工具内部推理 ' +
			details.toolCallCount +
			' 次 / ' +
			formatTokenCount(details.toolTotalTokens) +
			' tokens';
		if (details.toolCostUsd != null) {
			line += ' / 费用 ' + (formatCostUsd(details.toolCostUsd) || '');
		}
		parts.push(line);
	}
	if (details.model) parts.push(`模型 ${details.model}`);
	if (details.costUsd != null) parts.push(`费用 ${formatCostUsd(details.costUsd)}`);
	return parts.join('\n');
}
