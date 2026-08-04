import { get } from 'svelte/store';
import { addTaskMessage, activeTaskIdStore, moveTaskMessages, newMessage } from './stores.js';
import { invoke } from './tauri.js';

/**
 * Deliver a transcribed voice clip through the exact same path as a typed
 * message (`process_transcript`), so voice input continues the currently
 * open conversation instead of starting a new one.
 *
 * The optimistic voice message is appended first (under `_draft` when no
 * task is open). If the backend reports `TaskCreated`, the message is moved
 * out of wherever it landed — `_draft`, or a stale task id the UI
 * auto-restored while STT was running — into the fresh task, and the task is
 * focused. The page's `task:created` listener adopts `_draft` and switches
 * the view, so the move and the focus are idempotent either way.
 *
 * @param {string} text
 * @returns {Promise<any>} the `process_transcript` result
 */
export async function submitVoiceTranscript(text) {
	const activeId = get(activeTaskIdStore);
	const taskId = activeId || '_draft';
	addTaskMessage(
		taskId,
		newMessage({ role: 'user', content: text, voice: true, time: new Date().toLocaleTimeString() })
	);
	const result = await invoke('process_transcript', {
		transcript: text,
		activeTaskId: activeId || null,
		images: null,
	});
	if (result && result.TaskCreated && result.TaskCreated !== taskId) {
		moveTaskMessages(taskId, result.TaskCreated);
		activeTaskIdStore.set(result.TaskCreated);
	}
	return result;
}
