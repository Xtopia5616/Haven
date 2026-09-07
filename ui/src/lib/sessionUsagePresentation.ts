import {
	coalesceTokenTotal,
	cumulativeCacheHitRatePercent,
	formatCostUsd,
	formatTokenCount,
	type LlmUsage,
} from './sessionUsage';

export interface StepUsage {
	prompt: number;
	completion: number;
	total: number;
	cost: number;
	hasCost: boolean;
	durationMs: number;
	model: string | null;
	cacheMiss: number;
	cacheDiagnostics: LlmUsage['cache_diagnostics'] | null;
	calls: number;
}

export interface SessionTokenStats {
	promptTokens?: number;
	completionTokens?: number;
	totalTokens?: number;
	cachedTokens?: number;
	cacheCreationTokens?: number;
	cacheMissTokens?: number;
	contextTokens?: number;
	cacheExclusive?: boolean;
	cumulativePromptTokens?: number;
	cumulativeCompletionTokens?: number;
	cumulativeTotalTokens?: number;
	cumulativeCachedTokens?: number;
	cumulativeCacheCreationTokens?: number;
	cumulativeCostUsd?: number | null;
	contextWindow?: number | null;
	model?: string | null;
	estimated?: boolean;
	restored?: boolean;
}

export interface StepUsageMessage {
	type?: string | null;
	stepNumber?: number | null;
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
		if (/[\u3040-\u30ff\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff\uac00-\ud7af]/u.test(character)) {
			cjk += 1;
		} else {
			other += 1;
		}
	}
	return cjk + (other > 0 ? Math.ceil(other / 4) : 0);
}

/**
 * A ReAct step can contain several parallel tool cards, but the provider
 * reports one usage record for the model response that produced the batch.
 * Render that step aggregate on the first visible tool card only; repeating
 * it on every sibling makes one request look like several charges.
 */
export function isFirstToolForStep(
	messages: StepUsageMessage[],
	index: number,
): boolean {
	const current = messages[index];
	if (current?.type !== 'tool' || current.stepNumber == null) return false;
	return messages.findIndex(
		(message) => message.type === 'tool' && message.stepNumber === current.stepNumber,
	) === index;
}

/**
 * Aggregate persisted per-call usage for one ReAct step. The optional cache
 * lets the chat page avoid rebuilding the same tooltip data on every render.
 */
export function stepUsageFor(
	llmUsage: LlmUsage[],
	stepNumber: number | null,
	cache: Map<number, StepUsage>,
): StepUsage | null {
	if (stepNumber == null || llmUsage.length === 0) return null;
	const cached = cache.get(stepNumber);
	if (cached !== undefined) return cached;
	const calls = llmUsage.filter((usage) => usage.step_number === stepNumber);
	if (calls.length === 0) return null;
	const prompt = calls.reduce((sum, usage) => sum + (usage.prompt_tokens || 0), 0);
	const completion = calls.reduce((sum, usage) => sum + (usage.completion_tokens || 0), 0);
	const total = calls.reduce(
		(sum, usage) =>
			sum +
				coalesceTokenTotal(
					usage.prompt_tokens || 0,
					usage.completion_tokens || 0,
					usage.total_tokens || 0,
					usage.cached_tokens || 0,
					usage.cache_creation_tokens || 0,
					usage.cache_accounting || 'unknown',
				),
		0,
	);
	const value: StepUsage = {
		prompt,
		completion,
		total,
		cost: calls.reduce((sum, usage) => sum + (usage.cost_usd || 0), 0),
		hasCost: calls.some((usage) => usage.has_cost),
		durationMs: calls.reduce((sum, usage) => sum + (usage.duration_ms || 0), 0),
		model: calls.map((usage) => usage.model).filter(Boolean).at(-1) || null,
		cacheMiss: calls.reduce((sum, usage) => sum + (usage.cache_miss_tokens || 0), 0),
		cacheDiagnostics: calls.map((usage) => usage.cache_diagnostics).filter(Boolean).at(-1) || null,
		calls: calls.length,
	};
	cache.set(stepNumber, value);
	return value;
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

/** Build the tooltip for the chat token usage widget. */
export function buildTokenUsageTooltip(
	stats: SessionTokenStats,
	llmUsage: LlmUsage[],
): string {
	const parts: string[] = [];
	const cumulativePrompt = stats.cumulativePromptTokens || 0;
	const cumulativeCompletion = stats.cumulativeCompletionTokens || 0;
	const cumulativeCached = stats.cumulativeCachedTokens || 0;
	const cumulativeCreation = stats.cumulativeCacheCreationTokens || 0;
	const cumulativeTotal = coalesceTokenTotal(
		cumulativePrompt,
		cumulativeCompletion,
		stats.cumulativeTotalTokens || 0,
		cumulativeCached,
		cumulativeCreation,
	);
	if (stats.restored) {
		parts.push(`累计上传 ${cumulativePrompt} → 累计生成 ${cumulativeCompletion} tokens`);
		parts.push(`累计 ${cumulativeTotal} tokens`);
	} else {
		parts.push(
			`上传 ${stats.promptTokens || 0} → 生成 ${stats.completionTokens || 0} tokens`,
		);
		parts.push(`累计 ${cumulativeTotal} tokens`);
		if (stats.cumulativePromptTokens != null) {
			parts.push(
				`累计上传 ${stats.cumulativePromptTokens} → 累计生成 ${stats.cumulativeCompletionTokens} tokens`,
			);
		}
	}
	const liveCached = stats.cachedTokens || 0;
	const liveCreation = stats.cacheCreationTokens || 0;
	const liveMiss = stats.cacheMissTokens || 0;
	if (!stats.restored && (liveCached > 0 || liveCreation > 0)) {
		const rate = cacheHitRatePercent(
			stats.promptTokens || 0,
			liveCached,
			liveCreation,
			!!stats.cacheExclusive,
		);
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
	if (stats.model) parts.push(`模型 ${stats.model}`);
	if (stats.contextWindow) {
		const used = stats.contextTokens || stats.promptTokens || 0;
		const percentage = used ? `${((used / stats.contextWindow) * 100).toFixed(0)}%` : '?';
		parts.push(`上下文 ${percentage} / ${formatTokenCount(stats.contextWindow)}`);
	}
	if (stats.cumulativeCostUsd != null) parts.push(`费用 ${formatCostUsd(stats.cumulativeCostUsd)}`);
	if (stats.estimated) parts.push('估算值（历史对话，未计费）');
	return parts.join('\n');
}
