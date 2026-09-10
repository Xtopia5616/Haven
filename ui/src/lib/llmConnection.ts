/** Frontend boundary helpers for the typed LLM connectivity probe. */

export type LlmConnectionStatus = 'ready' | 'disconnected' | 'unconfigured';
export type LlmConnectionFailureReason =
	| 'network'
	| 'timeout'
	| 'authentication'
	| 'rate_limited'
	| 'server'
	| 'request_rejected'
	| 'invalid_response'
	| 'configuration'
	| 'unknown';

export interface LlmConnectionReport {
	status: LlmConnectionStatus;
	reason?: LlmConnectionFailureReason;
	provider?: string;
	model?: string;
}

const REASONS: Record<LlmConnectionFailureReason, string> = {
	network: '网络请求失败，可能与网络、代理、DNS 或 TLS 有关',
	timeout: '请求超时，请检查网络或服务是否可用',
	authentication: 'API Key 无效或没有访问权限',
	rate_limited: '模型服务限流，请稍后重试',
	server: '模型服务返回了服务器错误',
	request_rejected: '模型服务拒绝了请求，请检查地址和模型配置',
	invalid_response: '模型服务返回了无法识别的响应',
	configuration: '本地模型配置无效',
	unknown: '暂时无法确定具体原因',
};

export function llmConnectionReasonText(reason: unknown): string {
	return typeof reason === 'string' && reason in REASONS
		? REASONS[reason as LlmConnectionFailureReason]
		: REASONS.unknown;
}

/** Convert the untyped Tauri result into the one shape consumed by the shell. */
export function normalizeLlmConnectionReport(value: unknown): LlmConnectionReport {
	if (!value || typeof value !== 'object') {
		return { status: 'disconnected', reason: 'unknown' };
	}
	const report = value as Record<string, unknown>;
	const status = report.status;
	if (status !== 'ready' && status !== 'disconnected' && status !== 'unconfigured') {
		return { status: 'disconnected', reason: 'unknown' };
	}
	const reason = typeof report.reason === 'string' && report.reason in REASONS
		? (report.reason as LlmConnectionFailureReason)
		: undefined;
	return {
		status,
		reason,
		provider: typeof report.provider === 'string' ? report.provider : undefined,
		model: typeof report.model === 'string' ? report.model : undefined,
	};
}

export function formatLlmConnectionFailure(report: LlmConnectionReport): string {
	const identity = report.provider && report.model
		? `（${report.provider} / ${report.model}）`
		: '';
	return `默认模型${identity}连接失败：${llmConnectionReasonText(report.reason)}。请到模型设置检查 API 地址、API Key 和代理`;
}

export function formatLlmConnectionRecovery(report: LlmConnectionReport): string {
	const identity = report.provider && report.model
		? `（${report.provider} / ${report.model}）`
		: '';
	return `默认模型${identity}已恢复连接`;
}
