import type { MetricsSnapshot as GeneratedMetricsSnapshot } from './generatedCommands.ts';

/** Keep Rust-owned fields typed while allowing forward-compatible diagnostics. */
export type PerformanceMetricsSnapshot = GeneratedMetricsSnapshot & Record<string, unknown>;
