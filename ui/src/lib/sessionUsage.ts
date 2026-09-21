/** One LLM call's usage detail row. */
export interface LlmUsage {
	id?: string;
	step_number?: number | null;
	role?: string;
	call_kind: 'agent' | 'media' | 'tool' | string;
	model?: string | null;
	prompt_tokens?: number;
	completion_tokens?: number;
	total_tokens?: number;
	cached_tokens?: number;
	cache_creation_tokens?: number;
	cache_miss_tokens?: number;
	cache_accounting?: 'inclusive' | 'exclusive' | 'unknown' | string;
	context_tokens?: number;
	context_window?: number | null;
	cache_diagnostics?: {
		mode?: string;
		key_requested?: boolean;
		system_split?: boolean;
		downgraded?: boolean;
		outcome?: string;
	};
	cost_usd?: number | null;
	has_cost?: boolean;
	duration_ms?: number | null;
	created_at?: string;
}

/**
 * Reconstruct `total` when a provider omitted it. Matches
 * `Usage::normalize`: inclusive `prompt + completion`, plus an explicitly
 * exclusive cache read/write bucket. Unknown rows never guess from
 * cache token values.
 * @param {number} [prompt]
 * @param {number} [completion]
 * @param {number} [total]
 * @param {number} [cached]
 * @param {number} [creation]
 */
export function coalesceTokenTotal(
	prompt = 0,
	completion = 0,
	total = 0,
	cached = 0,
	creation = 0,
	cacheAccounting: string = 'unknown',
) {
	if (total) return total;
	const extra = cacheAccounting === 'exclusive' ? cached + creation : 0;
	return prompt + completion + extra || 0;
}

/**
 * Calculate a session's cache-hit rate from per-call provider contracts.
 * `prompt_tokens` already contains cache reads for inclusive providers, while
 * exclusive providers report them beside prompt tokens. Unknown rows
 * return null rather than silently using an incorrect aggregate denominator.
 */
export function cumulativeCacheHitRatePercent(calls: LlmUsage[]): number | null {
	const agentCalls = calls.filter((call) => call.call_kind === 'agent');
	if (
		!agentCalls.length ||
		agentCalls.some(
			(call) => !['inclusive', 'exclusive'].includes(call.cache_accounting || 'unknown'),
		)
	) {
		return null;
	}
	let cached = 0;
	let eligibleInput = 0;
	for (const call of agentCalls) {
		const prompt = call.prompt_tokens || 0;
		const read = call.cached_tokens || 0;
		const creation = call.cache_creation_tokens || 0;
		cached += read;
		eligibleInput += call.cache_accounting === 'exclusive' ? prompt + read + creation : prompt;
	}
	if (!eligibleInput) return null;
	return Math.min(100, Math.max(0, (cached / eligibleInput) * 100));
}

/**
 * Format a token count for compact display. Examples:
 *   123         -> "123"
 *   12_345      -> "12.3K"
 *   1_234_567   -> "1.23M"
 * @param {number} n
 * @returns {string}
 */
export function formatTokenCount(n: number) {
	const v = Number(n) || 0;
	if (v < 1_000) return String(v);
	if (v < 10_000) {
		const s = (v / 1_000).toFixed(2).replace(/\.?0+$/, '');
		return s + 'K';
	}
	if (v < 1_000_000) {
		const s = (v / 1_000).toFixed(1).replace(/\.0$/, '');
		return s + 'K';
	}
	return (v / 1_000_000).toFixed(2).replace(/\.?0+$/, '') + 'M';
}

/**
 * Format a USD cost. Examples:
 *   0          -> "$0.00"
 *   0.00123    -> "$0.0012"
 *   0.1234     -> "$0.123"
 *   1.5        -> "$1.50"
 * @param {number | null | undefined} v
 */
export function formatCostUsd(v: number | null | undefined) {
	if (v == null || !Number.isFinite(v)) return null;
	if (v === 0) return '$0.00';
	if (v < 0.01) return `$${v.toFixed(4)}`;
	if (v < 1) return `$${v.toFixed(3)}`;
	return `$${v.toFixed(2)}`;
}
