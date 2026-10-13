import { invoke } from '$lib/tauri.ts';

/** Tell the native startup surface that the hydrated app has painted. */
export async function reportStartupUiReady(): Promise<void> {
	await invoke('report_startup_ui_ready');
}
