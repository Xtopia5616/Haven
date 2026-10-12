import { writable } from 'svelte/store';
import type { AgentMediaPlanPayload } from './contracts/agent.ts';

/**
 * Live media-plan UI projections keyed by session. The backend also records a
 * snapshot-safe MediaPlan event in the ReAct event authority; this bounded
 * cache only keeps cards available while the current app run is visible.
 */
export const mediaPlanStore = writable<Record<string, AgentMediaPlanPayload[]>>({});

const MEDIA_PLAN_HISTORY_LIMIT = 32;
const MEDIA_PLAN_SESSION_LIMIT = 32;

export function rememberMediaPlan(payload: AgentMediaPlanPayload) {
	if (!payload.sessionId) return;
	const key = `${payload.stepNumber}:${payload.runId}:${payload.requestKind}`;
	mediaPlanStore.update((all) => {
		const previous = all[payload.sessionId] || [];
		const next = previous.filter(
			(plan) => `${plan.stepNumber}:${plan.runId}:${plan.requestKind}` !== key,
		);
		const updated = { ...all };
		// Reinsert so object order tracks recency for the bounded session cache.
		delete updated[payload.sessionId];
		updated[payload.sessionId] = [...next, payload].slice(-MEDIA_PLAN_HISTORY_LIMIT);
		const sessionIds = Object.keys(updated);
		while (sessionIds.length > MEDIA_PLAN_SESSION_LIMIT) {
			delete updated[sessionIds.shift()!];
		}
		return updated;
	});
}

export function clearMediaPlans(sessionId: string | null) {
	mediaPlanStore.update((all) => {
		if (!sessionId) return Object.keys(all).length ? {} : all;
		if (!(sessionId in all)) return all;
		const next = { ...all };
		delete next[sessionId];
		return next;
	});
}
