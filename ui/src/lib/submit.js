import { get } from 'svelte/store';
import {
	DRAFT_KEY,
	addTaskMessage,
	activeTaskIdStore,
	moveTaskMessages,
	newMessage,
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
 * @param {boolean} [opts.voice=false] - true when forwarded from a voice transcript
 * @returns {Promise<any>} the `process_transcript` result
 */
export async function submitTranscript(text, { images = null, voice = false } = {}) {
	const hasImages = Array.isArray(images) && images.length > 0;
	const activeId = get(activeTaskIdStore);
	const taskId = activeId || DRAFT_KEY;
	const msg = newMessage({
		role: 'user',
		content: text,
		voice,
		time: new Date().toLocaleTimeString(),
		...(hasImages ? { attachments: images, idPrefix: 'u' } : {}),
	});
	addTaskMessage(taskId, msg);
	try {
		const result = await invoke('process_transcript', {
			transcript: text,
			activeTaskId: activeId || null,
			images: hasImages ? images : null,
			voice,
		});
		if (result && result.TaskCreated && result.TaskCreated !== taskId) {
			moveTaskMessages(taskId, result.TaskCreated);
			activeTaskIdStore.set(result.TaskCreated);
		}
		return result;
	} catch (e) {
		updateTaskMessages(taskId, (list) => list.filter((x) => x.id !== msg.id));
		throw e;
	}
}
