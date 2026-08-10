import { get } from 'svelte/store';
import { browser } from '$app/environment';
import {
	DRAFT_KEY,
	NEW_TASK_INTENT_KEY,
	addTaskMessage,
	activeTaskIdStore,
	moveTaskMessages,
	newMessage,
	newTaskIntentStore,
	updateTaskMessages,
} from './stores.js';
import { invoke } from './tauri.js';

/**
 * Deliver a user submission (typed input or voice transcript) to the
 * backend through the same `process_transcript` path so voice input
 * continues the currently open conversation instead of starting a new one.
 *
 * The optimistic message is appended first under the active task id, or
 * under `_draft` when no task is open. If the backend replies with
 * `TaskCreated`, the message is migrated out of wherever it landed —
 * `_draft`, or a stale task id the UI auto-restored while the request was
 * in flight (meaningful for voice; harmless for typed) — into the fresh
 * task, and the active task id is updated.
 *
 * On submission failure the optimistic bubble is removed from the same
 * key it landed in and the error is rethrown so the caller can surface
 * it (notification toast, etc.).
 *
 * @param {string} text
 * @param {object} [opts]
 * @param {Array<{media_type: string, data: string}>} [opts.images=null] - image attachments; null/empty for voice
 * @param {Array<{media_type: string, data: string, filename: string}>} [opts.files=null] - non-image file attachments
 * @param {boolean} [opts.voice=false] - true when forwarded from a voice transcript
 * @returns {Promise<any>} the `process_transcript` result
 */
export async function submitTranscript(text, { images = null, files = null, voice = false } = {}) {
	const hasImages = Array.isArray(images) && images.length > 0;
	const hasFiles = Array.isArray(files) && files.length > 0;
	const hasAttachments = hasImages || hasFiles;
	// Images and files travel together as one attachment list; the backend
	// splits them again (images go to the vision model, files to disk).
	const attachments = [
		...(hasImages ? images : []),
		...(hasFiles ? files : []),
	];
	const activeId = get(activeTaskIdStore);
	const taskId = activeId || DRAFT_KEY;
	// Snapshot the fresh-start intent at DISPATCH time. If 新对话 is clicked
	// while this request is in flight, the intent is set AFTER this snapshot —
	// resolving must not clear it, or the click's blank draft would be
	// hijacked by this (older) submission. Only a submission that was already
	// part of the fresh-start flow may fulfill (clear) the intent.
	const freshStartAtDispatch = get(newTaskIntentStore);
	const msg = newMessage({
		role: 'user',
		content: text,
		voice,
		time: new Date().toLocaleTimeString(),
		...(hasAttachments ? { attachments, idPrefix: 'u' } : {}),
	});
	addTaskMessage(taskId, msg);
	// A reviewed conversation with no persisted messages yet (e.g. after
	// rolling back the very first user message) is rebuilt with a
	// display-only `placeholder-*` bubble carrying the task input text.
	// The submitted message is the real start of the conversation: drop
	// the stand-in so the original input is never shown twice.
	updateTaskMessages(taskId, (list) => {
		if (!list.some((m) => m.id.startsWith('placeholder-'))) return list;
		return list.filter((m) => !m.id.startsWith('placeholder-'));
	});
	try {
		const result = await invoke('process_transcript', {
			transcript: text,
			activeTaskId: activeId || null,
			attachments: hasAttachments ? attachments : null,
			voice,
		});
		if (result && result.TaskCreated && result.TaskCreated !== taskId) {
			// This submission created the new task. Clear the fresh-start intent
			// ONLY if the fresh-start was already active when this submission was
			// dispatched — otherwise a 新对话 click that landed mid-flight must
			// not be cancelled, or the blank draft would be hijacked by this
			// (older) submission. Cleared BEFORE the store write so the page's
			// store→state follow effect (guarded by the intent) can adopt the
			// new task when appropriate.
			if (freshStartAtDispatch) {
				newTaskIntentStore.set(false);
				if (browser) localStorage.removeItem(NEW_TASK_INTENT_KEY);
			}
			moveTaskMessages(taskId, result.TaskCreated);
			activeTaskIdStore.set(result.TaskCreated);
		}
		return result;
	} catch (e) {
		updateTaskMessages(taskId, (list) => list.filter((x) => x.id !== msg.id));
		throw e;
	}
}
