import { writable, get } from 'svelte/store';

/** True while SettingsView has unsaved edits. */
export const settingsDirty = writable(false);

export type SettingsLeaveGuard = {
	/** Prompt the user; resolve true to allow leaving the settings tab. */
	confirmLeave: () => Promise<boolean>;
};

let guard: SettingsLeaveGuard | null = null;

/** @param {SettingsLeaveGuard | null} g */
export function registerSettingsLeaveGuard(g: SettingsLeaveGuard | null) {
	guard = g;
	if (!g) settingsDirty.set(false);
}

/**
 * If settings are dirty, run the registered leave prompt.
 * @returns {Promise<boolean>} true when navigation away may proceed
 */
export async function confirmLeaveSettingsIfNeeded(): Promise<boolean> {
	if (!get(settingsDirty) || !guard) return true;
	return guard.confirmLeave();
}
