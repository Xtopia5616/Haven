import { writable } from 'svelte/store';
import type { StatusTone } from './statusColors.ts';

export type NotificationType = Extract<StatusTone, 'info' | 'success' | 'warning' | 'error'>;

export type Notification = {
	id: string;
	msg: string;
	type: NotificationType;
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
) {
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
