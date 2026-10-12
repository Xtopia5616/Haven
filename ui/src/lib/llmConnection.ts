/** Frontend boundary helpers for the typed LLM connectivity probe. */

import {
	LLM_CONNECTION_FAILURE_REASON_VALUES,
	LLM_CONNECTION_STATUS_VALUES,
	type LlmConnectionFailureReason,
	type LlmConnectionReport,
	type LlmConnectionStatus,
} from './contracts/generatedCommands.ts';
import { isRecord } from './contracts/objectGuards.ts';
import { isOneOf, isString } from './contracts/valueGuards.ts';

export interface LlmConnectionReportView {
	status: LlmConnectionStatus;
	reason?: LlmConnectionFailureReason;
	provider: string;
	model: string;
}

const INVALID_REPORT_FALLBACK: LlmConnectionReportView = {
	status: 'disconnected',
	reason: 'unknown',
	provider: '',
	model: '',
};

function invalidReport(): LlmConnectionReportView {
	return { ...INVALID_REPORT_FALLBACK };
}

const REASONS: Record<LlmConnectionFailureReason, string> = {
	network: '网络请求失败，可能与网络、代理、DNS 或 TLS 有关',
	timeout: '请求超时，请检查网络或服务是否可用',
	authentication: 'API Key 无效或没有访问权限',
	rate_limited: '模型服务限流，请稍后重试',
	circuit_open: '连续请求失败后，模型调用暂时进入保护状态',
	server: '模型服务返回了服务器错误',
	request_rejected: '模型服务拒绝了请求，请检查地址和模型配置',
	invalid_response: '模型服务返回了无法识别的响应',
	configuration: '本地模型配置无效',
	unknown: '暂时无法确定具体原因',
};

export function llmConnectionReasonText(reason: unknown): string {
	return typeof reason === 'string' && Object.hasOwn(REASONS, reason)
		? REASONS[reason as LlmConnectionFailureReason]
		: REASONS.unknown;
}

/** Convert the untyped Tauri result into the one shape consumed by the shell. */
export function normalizeLlmConnectionReport(value: unknown): LlmConnectionReportView {
	if (
		!isRecord(value) ||
		!Object.hasOwn(value, 'status') ||
		!isOneOf(value.status, LLM_CONNECTION_STATUS_VALUES) ||
		!Object.hasOwn(value, 'provider') ||
		!isString(value.provider) ||
		!Object.hasOwn(value, 'model') ||
		!isString(value.model)
	) {
		return invalidReport();
	}

	const status = value.status;
	const provider = value.provider;
	const model = value.model;
	let report: LlmConnectionReport;
	if (status === 'disconnected') {
		if (
			!Object.hasOwn(value, 'reason') ||
			!isOneOf(value.reason, LLM_CONNECTION_FAILURE_REASON_VALUES)
		) {
			return invalidReport();
		}
		report = { status, reason: value.reason, provider, model };
	} else {
		if (Object.hasOwn(value, 'reason')) {
			return invalidReport();
		}
		report = { status, provider, model };
	}

	// Keep the shell-facing view explicit at the generated IPC DTO boundary.
	return report.status === 'disconnected'
		? {
				status: report.status,
				reason: report.reason,
				provider: report.provider,
				model: report.model,
			}
		: { status: report.status, provider: report.provider, model: report.model };
}

export function formatLlmConnectionFailure(report: LlmConnectionReportView): string {
	const identity =
		report.provider && report.model ? `（${report.provider} / ${report.model}）` : '';
	if (report.reason === 'circuit_open') {
		return `默认模型${identity}处于保护状态，点击“继续生成”可立即重试`;
	}
	return `默认模型${identity}连接失败：${llmConnectionReasonText(report.reason)}。请到模型设置检查 API 地址、API Key 和代理`;
}

export function formatLlmConnectionRecovery(report: LlmConnectionReportView): string {
	const identity =
		report.provider && report.model ? `（${report.provider} / ${report.model}）` : '';
	return `默认模型${identity}已恢复连接`;
}
