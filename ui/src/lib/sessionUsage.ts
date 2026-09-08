import { writable } from 'svelte/store';

/**
 * Per-session token usage + cost reported by the agent. Keyed by session id.
 * Updated on every `agent:usage` event so the chat toolbar can show
 * running totals and remaining context budget.
 *
 * Shape: { [sessionId: string]: {
 *   promptTokens, completionTokens, totalTokens,
 *   cumulativePromptTokens, cumulativeCompletionTokens, cumulativeTotalTokens,
 *   costUsd, cumulativeCostUsd, contextWindow, model,
 *   lastUpdated: number,
 * }}
 */
export const sessionTokenStatsStore = writable<Record<string, any>>({});

/** One LLM call's usage detail row. */
export interface LlmUsage {
	id?: string;
	step_number?: number | null;
	role?: string;
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
 * Update (or insert) the token-stats entry for a session. Replaces the whole
 * session entry so stale fields don't accumulate across event variants.
 */
export function updateSessionTokenStats(sessionId: string, stats: Record<string, unknown>) {
	if (!sessionId) return;
	sessionTokenStatsStore.update((m) => ({
		...m,
		[sessionId]: { ...(m[sessionId] || {}), ...stats, lastUpdated: Date.now() },
	}));
}

/** Clear token stats for a finished/reset session. */
export function clearSessionTokenStats(sessionId: string) {
	if (!sessionId) return;
	sessionTokenStatsStore.update((m) => {
		if (!(sessionId in m)) return m;
		const next = { ...m };
		delete next[sessionId];
		return next;
	});
}

/**
 * Restore token stats for a session from persisted backend usage counters
 * (returned by get_session_for_resume / get_last_conversation). Cumulative
 * totals are the persisted running totals; per-step and budget fields stay
 * empty until the next `agent:usage` event. `restored` marks the entry as
 * coming from persistence (a resume / reopened conversation): no further
 * `agent:usage` events may arrive, so the widget falls back to showing the
 * cumulative total instead of the per-step context count. When the session
 * predates usage persistence, `estimated` marks the restored totals as a
 * rough estimate derived from the persisted conversation text (no cost).
 * @param {string} sessionId
 * @param {object} usage - { prompt_tokens, completion_tokens, total_tokens, cached_tokens, cache_creation_tokens, cost_usd, has_cost }
 * @param {boolean} [estimated]
 */
export function restoreSessionTokenStats(
	sessionId: string,
	usage: {
		prompt_tokens?: number;
		completion_tokens?: number;
		total_tokens?: number;
		cached_tokens?: number;
		cache_creation_tokens?: number;
		cache_miss_tokens?: number;
		context_tokens?: number;
		context_window?: number | null;
		cost_usd?: number | null;
		has_cost?: boolean;
	},
	estimated = false,
) {
	if (!sessionId || !usage) return;
	const hasCost = !!usage.has_cost && usage.cost_usd != null;
	const prompt = usage.prompt_tokens || 0;
	const completion = usage.completion_tokens || 0;
	const cached = usage.cached_tokens || 0;
	const creation = usage.cache_creation_tokens || 0;
	const miss = usage.cache_miss_tokens || 0;
	updateSessionTokenStats(sessionId, {
		promptTokens: 0,
		completionTokens: 0,
		totalTokens: 0,
		cachedTokens: 0,
		cacheCreationTokens: 0,
		cacheMissTokens: 0,
		contextTokens: usage.context_tokens || 0,
		cacheExclusive: false,
		cumulativePromptTokens: prompt,
		cumulativeCompletionTokens: completion,
		cumulativeTotalTokens: coalesceTokenTotal(
			prompt,
			completion,
			usage.total_tokens || 0,
			cached,
			creation,
		),
		cumulativeCachedTokens: cached,
		cumulativeCacheCreationTokens: creation,
		cumulativeCacheMissTokens: miss,
		costUsd: null,
		cumulativeCostUsd: hasCost ? usage.cost_usd : null,
		contextWindow: usage.context_window ?? null,
		model: null,
		estimated: !!estimated,
		restored: true,
	});
}

/**
 * Per-session per-LLM-call usage detail (restored from get_session_for_resume /
 * get_last_conversation `llm_usage`), keyed by session id. Each entry is one
 * model response: { step_number, role, model, prompt_tokens,
 * completion_tokens, total_tokens, cost_usd, has_cost, duration_ms,
 * created_at }.
 * @type {import('svelte/store').Writable<Record<string, Array<object>>>}
 */
export const sessionLlmUsageStore = writable<Record<string, LlmUsage[]>>({});

/**
 * Restore the per-call usage-detail list for a session (from
 * `get_session_for_resume` / `get_last_conversation`). An EMPTY array overwrites
 * too: after a rollback truncates the usage rows the backend returns `[]`,
 * and the stale detail for discarded steps must not linger in the store
 * (mirrors restoreSessionTokenStats's unconditional overwrite). Only
 * `undefined` (backend predates the field) is ignored.
 */
export function restoreSessionLlmUsage(sessionId: string, usageList: LlmUsage[]) {
	if (!sessionId || !Array.isArray(usageList)) return;
	sessionLlmUsageStore.update((m) => ({ ...m, [sessionId]: usageList }));
}

/**
 * Append one live `agent:usage` call onto the per-session detail list so
 * tool-card token chips update during the run (not only after resume restore).
 */
export function appendSessionLlmUsage(sessionId: string, entry: LlmUsage) {
	if (!sessionId) return;
	sessionLlmUsageStore.update((m) => {
		const prev = m[sessionId] || [];
		return { ...m, [sessionId]: [...prev, entry] };
	});
}

/** Clear per-call usage detail for a finished/reset session. */
export function clearSessionLlmUsage(sessionId: string) {
	if (!sessionId) return;
	sessionLlmUsageStore.update((m) => {
		if (!(sessionId in m)) return m;
		const next = { ...m };
		delete next[sessionId];
		return next;
	});
}

/**
 * Reconstruct `total` when a provider omitted it. Matches
 * `Usage::normalize`: inclusive `prompt + completion`, plus an explicitly
 * exclusive cache read/write bucket. Unknown legacy rows never guess from
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
 * exclusive providers report them beside prompt tokens. Unknown legacy rows
 * return null rather than silently using an incorrect aggregate denominator.
 */
export function cumulativeCacheHitRatePercent(calls: LlmUsage[]): number | null {
	if (
		!calls.length ||
		calls.some(
			(call) => !['inclusive', 'exclusive'].includes(call.cache_accounting || 'unknown'),
		)
	) {
		return null;
	}
	let cached = 0;
	let eligibleInput = 0;
	for (const call of calls) {
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
