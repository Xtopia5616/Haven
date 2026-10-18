import { invoke } from '$lib/tauri.ts';
import { parseBooleanResponse } from './contracts/boolean.ts';
import { mapSchedulePlans } from './contracts/schedule.ts';

export async function listSchedules() {
	const value: unknown = await invoke('list_schedules');
	return mapSchedulePlans(value);
}

export function cancelSchedule(scheduleId: string, sessionId: string): Promise<boolean> {
	return invoke('cancel_schedule', { scheduleId, sessionId }).then((value: unknown) =>
		parseBooleanResponse(value, 'cancel_schedule'),
	);
}
