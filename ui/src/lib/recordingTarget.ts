import { get } from 'svelte/store';
import { appSessionReducer } from './sessionReducer.ts';
import { newSessionIntentStore } from './sessionIntentStore.ts';
import { invoke } from './tauri.ts';

export interface RecordingTargetSnapshot {
	sessionId: string | null;
	selectionVersion: number;
	revision: number;
}

// Include wall-clock time so a webview reload in the same Rust process starts
// above the previous renderer's revision; the local increment disambiguates
// multiple captures or rapid switches within one millisecond.
let revision = Math.floor(Date.now() * 1000);

function nextRevision() {
	revision = Math.max(revision + 1, Math.floor(Date.now() * 1000));
	return revision;
}

export function snapshotRecordingTarget(): RecordingTargetSnapshot {
	return {
		sessionId: get(newSessionIntentStore) ? null : appSessionReducer.snapshot().activeSessionId,
		selectionVersion: appSessionReducer.currentUserSelectionVersion(),
		revision: nextRevision(),
	};
}

export async function syncRecordingTarget(): Promise<RecordingTargetSnapshot> {
	const target = snapshotRecordingTarget();
	await invoke('set_recording_target_session', target);
	return target;
}
