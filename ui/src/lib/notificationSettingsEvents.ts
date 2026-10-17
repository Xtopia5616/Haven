import type { NotificationConfigInput } from './contracts/generatedCommands.ts';

const NOTIFICATION_SETTINGS_CHANGED = 'haven:notification-settings-changed';

export function shouldApplyInitialNotificationSnapshot(
	loadRevision: number,
	currentRevision: number,
): boolean {
	return loadRevision === currentRevision;
}

export function isNotificationInAppEnabled(
	notification: NotificationConfigInput,
	kind: keyof NotificationConfigInput,
): boolean {
	return notification[kind]?.in_app !== false;
}

/** Publish a successfully persisted notification snapshot to the layout owner. */
export function publishNotificationSettingsChanged(notification: NotificationConfigInput): void {
	if (typeof window === 'undefined') return;
	window.dispatchEvent(
		new CustomEvent<NotificationConfigInput>(NOTIFICATION_SETTINGS_CHANGED, {
			detail: notification,
		}),
	);
}

/** Subscribe to notification config changes without creating another state owner. */
export function onNotificationSettingsChanged(
	handler: (notification: NotificationConfigInput) => void,
): () => void {
	if (typeof window === 'undefined') return () => {};
	const listener = (event: Event) => {
		if (event instanceof CustomEvent) handler(event.detail as NotificationConfigInput);
	};
	window.addEventListener(NOTIFICATION_SETTINGS_CHANGED, listener);
	return () => window.removeEventListener(NOTIFICATION_SETTINGS_CHANGED, listener);
}
