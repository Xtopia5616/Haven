import { invoke } from './tauri.ts';
import type {
	CheckShellAvailableRequest,
	ReadLogTailRequest,
	UiMetricsSnapshot,
} from './contracts/commands.ts';
import {
	parseApiKeyStatus,
	parseLogInfo,
	parseLogTail,
	parseShellAvailability,
	type ApiKeyStatus,
	type LogInfo,
	type LogTail,
	type MetricsSnapshot,
	type ShellAvailability,
} from './contracts/settings.ts';

/** Read log metadata using the existing settings response validator. */
export function getLogInfo(): Promise<LogInfo> {
	return invoke('get_log_info').then(parseLogInfo);
}

/** Read a bounded log tail using the existing settings response validator. */
export function readLogTail(request: ReadLogTailRequest): Promise<LogTail> {
	return invoke('read_log_tail', request).then(parseLogTail);
}

/** Check availability for one configured shell through the settings validator. */
export function checkShellAvailable(request: CheckShellAvailableRequest): Promise<ShellAvailability> {
	return invoke('check_shell_available', request).then(parseShellAvailability);
}

/** Read credential-presence flags only; provider and model identifiers stay dynamic. */
export function getApiKeyStatus(): Promise<ApiKeyStatus> {
	return invoke('get_api_key_status').then(parseApiKeyStatus);
}

/** Read content-free performance metrics without filtering dynamic diagnostic fields. */
export function getPerformanceMetrics(ui?: UiMetricsSnapshot): Promise<MetricsSnapshot> {
	return invoke('get_performance_metrics', ui ? { ui } : undefined);
}
