import { addNotification, NOTIFICATION_DURATIONS } from './notificationStore.ts';
import type { NotificationType } from './notificationStore.ts';
import { formatError } from './formatError.ts';
import logger from './logger.ts';
import { invoke } from './tauri.ts';
import type { FrontendFailureKindInput } from './contracts/generatedCommands.ts';

const loggedErrors = new WeakSet<object>();

export type ErrorReportOptions = {
	/** Full user-facing prefix, for example `保存设置失败`. */
	message: string;
	/** Short module name used in the frontend log. */
	context: string;
	/** Keep false when a lower-level boundary already logged the error. */
	log?: boolean;
	/** Suppress the toast for background/diagnostic failures. */
	notify?: boolean;
	/** Hide technical details from the toast; logs never include error details. */
	includeDetail?: boolean;
	/** Exact user-facing toast text for global errors. */
	notificationMessage?: string;
	/** Use warning when the failed operation left a recoverable partial state. */
	notificationType?: Extract<NotificationType, 'error' | 'warning'>;
	/** Stable category mirrored to Rust; never contains error or payload text. */
	failureKind?: FrontendFailureKindInput;
};

function markLogged(error: unknown) {
	if (error && typeof error === 'object') loggedErrors.add(error);
}

function wasLogged(error: unknown): boolean {
	return !!(error && typeof error === 'object' && loggedErrors.has(error));
}

/** Log one fixed error summary per object at the UI boundary. */
export function logError(context: string, error: unknown): void {
	if (wasLogged(error)) return;
	logger.error(context, 'operation failed');
	markLogged(error);
}

/**
 * Report a recoverable UI failure through the one canonical error path.
 * `invoke()` marks its low-level failures, so a page catch adds the toast
 * without duplicating the same error record.
 */
export function reportError(error: unknown, options: ErrorReportOptions): string {
	const detail = formatError(error);
	const safeMessage = formatError(options.message);
	const message = formatError(
		options.includeDetail === false || detail === '未知错误'
			? safeMessage
			: `${safeMessage}: ${detail}`,
	);
	const notificationMessage = formatError(options.notificationMessage ?? message);
	if (options.log !== false && !wasLogged(error)) {
		logError(options.context, error);
		void invoke('log_frontend_error', {
			failureKind: options.failureKind ?? 'operation_failure',
		}).catch(() => {});
	}
	if (options.notify !== false) {
		const type = options.notificationType ?? 'error';
		addNotification(notificationMessage, type, NOTIFICATION_DURATIONS[type]);
	}
	return notificationMessage;
}

/** Install the last-resort handlers for errors which escaped component code. */
export function installGlobalErrorHandlers() {
	if (typeof window === 'undefined') return () => {};
	const onError = (event: ErrorEvent) => {
		reportError(event.error || event.message, {
			context: 'global',
			message: 'Unhandled UI error',
			includeDetail: false,
			failureKind: 'unhandled_ui_error',
			notificationMessage: '应用发生未处理错误，请重试',
		});
	};
	const onUnhandledRejection = (event: PromiseRejectionEvent) => {
		reportError(event.reason, {
			context: 'global',
			message: 'Unhandled promise rejection',
			includeDetail: false,
			failureKind: 'unhandled_rejection',
			notificationMessage: '应用操作未完成，请重试',
		});
	};
	window.addEventListener('error', onError);
	window.addEventListener('unhandledrejection', onUnhandledRejection);
	return () => {
		window.removeEventListener('error', onError);
		window.removeEventListener('unhandledrejection', onUnhandledRejection);
	};
}
