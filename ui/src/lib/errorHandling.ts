import { addNotification, NOTIFICATION_DURATIONS } from './stores.ts';
import { formatError } from './formatError.ts';
import logger from './logger.ts';

const LOGGED_ERROR = Symbol('haven.loggedError');

type LoggableError = object & { [LOGGED_ERROR]?: boolean };

export type ErrorReportOptions = {
	/** Full user-facing prefix, for example `保存设置失败`. */
	message: string;
	/** Short module name used in the frontend log. */
	context: string;
	/** Keep false when a lower-level boundary already logged the error. */
	log?: boolean;
	/** Suppress the toast for background/diagnostic failures. */
	notify?: boolean;
};

function markLogged(error: unknown) {
	if (error && typeof error === 'object') {
		try {
			Object.defineProperty(error, LOGGED_ERROR, { value: true, configurable: true });
		} catch {
			// Frozen errors are still safe to log; they simply cannot be marked.
		}
	}
}

function wasLogged(error: unknown): boolean {
	return !!(error && typeof error === 'object' && (error as LoggableError)[LOGGED_ERROR]);
}

/** Log an error once at the UI boundary using the shared context format. */
export function logError(context: string, message: string, error: unknown): void {
	if (wasLogged(error)) return;
	logger.error(context, message, formatError(error));
	markLogged(error);
}

/**
 * Report a recoverable UI failure through the one canonical error path.
 * `invoke()` marks its low-level failures, so a page catch adds the toast
 * without duplicating the same error record.
 */
export function reportError(error: unknown, options: ErrorReportOptions): string {
	const detail = formatError(error);
	const message = detail === '未知错误' ? options.message : `${options.message}: ${detail}`;
	if (options.log !== false) logError(options.context, `${options.message} failed`, error);
	if (options.notify !== false) {
		addNotification(message, 'error', NOTIFICATION_DURATIONS.error, { logError: false });
	}
	return message;
}

/** Install the last-resort handlers for errors which escaped component code. */
export function installGlobalErrorHandlers() {
	if (typeof window === 'undefined') return () => {};
	const onError = (event: ErrorEvent) => {
		logError('global', 'Unhandled UI error', event.error || event.message);
		addNotification('应用发生未处理错误，请重试', 'error', NOTIFICATION_DURATIONS.error, {
			logError: false,
		});
	};
	const onUnhandledRejection = (event: PromiseRejectionEvent) => {
		logError('global', 'Unhandled promise rejection', event.reason);
		addNotification('应用操作未完成，请重试', 'error', NOTIFICATION_DURATIONS.error, {
			logError: false,
		});
	};
	window.addEventListener('error', onError);
	window.addEventListener('unhandledrejection', onUnhandledRejection);
	return () => {
		window.removeEventListener('error', onError);
		window.removeEventListener('unhandledrejection', onUnhandledRejection);
	};
}
