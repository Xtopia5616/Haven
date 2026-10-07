import { readPerformanceMetricsSnapshot } from './diagnosticsCommands.ts';
import type { PerformanceMetricsSnapshot } from './contracts/diagnostics.ts';
import type { UiMetricsSnapshot } from './contracts/commands.ts';

let uiMetricsProvider: (() => UiMetricsSnapshot) | null = null;

/** Register the page-owned renderer counters with the unified diagnostics API. */
export function registerPerformanceMetricsProvider(
	provider: () => UiMetricsSnapshot,
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
