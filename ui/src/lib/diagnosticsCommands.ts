import { invoke } from './tauri.ts';
import {
	parseApiKeyStatus,
	parseLogInfo,
	parseLogTail,
	parsePerformanceMetricsSnapshot,
	parseShellAvailability,
	type ApiKeyStatus,
	type LogInfo,
	type LogTail,
	type PerformanceMetricsSnapshot,
	type ShellAvailability,
} from './contracts/diagnostics.ts';
import type { TauriCommandRequest, UiMetricsSnapshot } from './contracts/generatedCommands.ts';

/** Read log metadata through the diagnostics response boundary. */
export function readLogInfo(): Promise<LogInfo> {
	return invoke('get_log_info').then(parseLogInfo);
}

/** Read a bounded log tail through the diagnostics response boundary. */
export function readLogTail(
	request: TauriCommandRequest<'read_log_tail'>,
): Promise<LogTail> {
	return invoke('read_log_tail', request).then(parseLogTail);
}

/** Check availability for one configured shell through the diagnostics response boundary. */
export function checkShellAvailable(
	request: TauriCommandRequest<'check_shell_available'>,
): Promise<ShellAvailability> {
	return invoke('check_shell_available', request).then(parseShellAvailability);
}

/** Read credential-presence flags only; provider and model identifiers stay dynamic. */
export function readApiKeyStatus(): Promise<ApiKeyStatus> {
	return invoke('get_api_key_status').then(parseApiKeyStatus);
}

/** Read metrics and reject Rust u64 values that JavaScript cannot represent exactly. */
export function requestPerformanceMetricsSnapshot(
	ui?: UiMetricsSnapshot,
): Promise<PerformanceMetricsSnapshot> {
	return invoke('get_performance_metrics', ui ? { ui } : undefined).then((value: unknown) =>
		parsePerformanceMetricsSnapshot(value),
	);
}
