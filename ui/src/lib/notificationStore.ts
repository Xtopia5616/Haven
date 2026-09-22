import { writable } from 'svelte/store';
import { invoke } from './tauri.ts';
import logger from '$lib/logger.ts';

export type NotificationType = 'info' | 'success' | 'warning' | 'error';

export type Notification = {
	id: string;
	msg: string;
	type: NotificationType;
};

export type NotificationOptions = {
	/** Internal escape hatch for reportError, which owns the log entry. */
	logError?: boolean;
};

export const notificationStore = writable<Notification[]>([]);

export const NOTIFICATION_DURATIONS: Record<NotificationType, number> = {
	info: 3000,
	success: 3000,
	warning: 4000,
	error: 5000,
};

let notificationSeq = 0;

export function addNotification(
	msg: string,
	type: NotificationType = 'info',
	duration = NOTIFICATION_DURATIONS[type],
	options: NotificationOptions = {},
) {
	if (type === 'error' && options.logError !== false) {
		logger.error('notification', msg);
	}
	if (type === 'error') {
		// Error toasts are also mirrored into the Rust file log. This is best
		// effort so the notification remains usable during an upgrade.
		void invoke('log_frontend_error', { message: msg }).catch(() => {});
	}
	let id: string | null = null;
	notificationStore.update((current) => {
		if (current.some((item) => item.msg === msg && item.type === type)) {
			return current;
		}
		id = `${Date.now()}-${notificationSeq++}-${Math.random().toString(36).slice(2, 6)}`;
		return [...current, { id, msg, type }];
	});
	if (id !== null) {
		setTimeout(() => {
			notificationStore.update((current) => current.filter((item) => item.id !== id));
		}, duration);
	}
}
