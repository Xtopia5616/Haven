import { readPerformanceMetricsSnapshot } from './diagnosticsCommands.ts';
import type { PerformanceMetricsSnapshot } from './contracts/diagnostics.ts';
import type { StreamMetricsSnapshot } from './streamAggregator.ts';

let uiMetricsProvider: (() => StreamMetricsSnapshot) | null = null;

/** Register the page-owned renderer counters with the unified diagnostics API. */
export function registerPerformanceMetricsProvider(
	provider: () => StreamMetricsSnapshot,
): () => void {
	uiMetricsProvider = provider;
	return () => {
		if (uiMetricsProvider === provider) uiMetricsProvider = null;
	};
}

/** Read backend and renderer metrics through one Tauri diagnostics boundary. */
export function getPerformanceMetrics(): Promise<PerformanceMetricsSnapshot> {
	const ui = uiMetricsProvider?.();
	return readPerformanceMetricsSnapshot(ui);
}
