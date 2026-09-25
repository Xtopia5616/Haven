import { parseSettingsPayload, type SettingsPayload } from '$lib/contracts/settings.ts';
import { invoke } from '$lib/tauri.ts';

/** Load Settings through the single runtime boundary used by renderer callers. */
export function loadSettings(): Promise<SettingsPayload | null> {
	return invoke('get_settings').then(parseSettingsPayload);
}
