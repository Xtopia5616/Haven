import { coalesceTokenTotal } from '../sessionUsage.ts';
import type {
	ResumeUsage,
	SessionActionOf,
	SessionReducerState,
	SessionTokenStats,
} from './types.ts';

type UsageReducerAction = SessionActionOf<
	'sessions/cleared' | 'session/usage-restored' | 'session/usage-live' | 'session/usage-cleared'
>;

function restoreTokenStats(usage: ResumeUsage): SessionTokenStats {
	const prompt = usage.prompt_tokens || 0;
	const completion = usage.completion_tokens || 0;
	const cached = usage.cached_tokens || 0;
	const creation = usage.cache_creation_tokens || 0;
	return {
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
		cumulativeCacheMissTokens: usage.cache_miss_tokens || 0,
		costUsd: null,
		cumulativeCostUsd: usage.has_cost && usage.cost_usd != null ? usage.cost_usd : null,
		contextWindow: usage.context_window ?? null,
		model: null,
		restored: true,
		lastUpdated: Date.now(),
	};
}
export function reduceUsage(
	inputState: SessionReducerState,
	action: UsageReducerAction,
): SessionReducerState {
	const state = inputState;
	switch (action.type) {
		case 'sessions/cleared':
			return { ...state, tokenStats: {}, llmUsage: {} };
		case 'session/usage-restored':
			if (!action.usage)
				return action.llmUsage
					? {
							...state,
							llmUsage: {
								...state.llmUsage,
								[action.sessionId]: action.llmUsage,
							},
						}
					: state;
			return {
				...state,
				tokenStats: {
					...state.tokenStats,
					[action.sessionId]: restoreTokenStats(action.usage),
				},
				llmUsage: action.llmUsage
					? { ...state.llmUsage, [action.sessionId]: action.llmUsage }
					: state.llmUsage,
			};
		case 'session/usage-live': {
			const tokenStats = action.stats
				? {
						...state.tokenStats,
						[action.sessionId]: {
							...action.stats,
							restored: false,
							lastUpdated: Date.now(),
						},
					}
				: state.tokenStats;
			const llmUsage = action.call
				? {
						...state.llmUsage,
						[action.sessionId]: [
							...(state.llmUsage[action.sessionId] || []),
							action.call,
						],
					}
				: state.llmUsage;
			return { ...state, tokenStats, llmUsage };
		}
		case 'session/usage-cleared': {
			const tokenStats = { ...state.tokenStats };
			const llmUsage = { ...state.llmUsage };
			delete tokenStats[action.sessionId];
			delete llmUsage[action.sessionId];
			return { ...state, tokenStats, llmUsage };
		}
	}
	return inputState;
}
