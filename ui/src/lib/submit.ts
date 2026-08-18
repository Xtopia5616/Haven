import { get } from 'svelte/store';
import { browser } from '$app/environment';
import {
	DRAFT_KEY,
	NEW_ACTION_INTENT_KEY,
	addSessionMessage,
	activeSessionIdStore,
	moveSessionMessages,
	newMessage,
	newSessionIntentStore,
	updateSessionMessages,
} from './stores.ts';
import { invoke } from './tauri.ts';

/**
 * Deliver a user submission (typed input or voice transcript) to the
 * backend through the same `process_transcript` path so voice input
 * continues the currently open conversation instead of starting a new one.
 *
 * The optimistic message is appended first under the active session id, or
 * under `_draft` when no session is open. If the backend replies with
 * `SessionCreated`, the message is migrated out of wherever it landed —
 * `_draft`, or a stale session id the UI auto-restored while the request was
 * in flight (meaningful for voice; harmless for typed) — into the fresh
 * session, and the active session id is updated.
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
interface SubmitOptions {
	images?: Array<{ media_type: string; data: string }> | null;
	files?: Array<{ media_type: string; data: string; filename: string }> | null;
	voice?: boolean;
}

export async function submitTranscript(
	text: string,
	{ images = null, files = null, voice = false }: SubmitOptions = {},
): Promise<any> {
	const hasImages = Array.isArray(images) && images.length > 0;
	const hasFiles = Array.isArray(files) && files.length > 0;
	const hasAttachments = hasImages || hasFiles;
	// Images and files travel together as one attachment list; the backend
	// splits them again (images go to the vision model, files to disk).
	const attachments = [
		...(hasImages ? images : []),
		...(hasFiles ? files : []),
	];
	const activeId = get(activeSessionIdStore);
	const sessionId = activeId || DRAFT_KEY;
	// Snapshot the fresh-start intent at DISPATCH time. If 新对话 is clicked
	// while this request is in flight, the intent is set AFTER this snapshot —
	// resolving must not clear it, or the click's blank draft would be
	// hijacked by this (older) submission. Only a submission that was already
	// part of the fresh-start flow may fulfill (clear) the intent.
	const freshStartAtDispatch = get(newSessionIntentStore);
	const msg = newMessage({
		role: 'user',
		content: text,
		voice,
		time: new Date().toLocaleTimeString(),
		...(hasAttachments ? { attachments, idPrefix: 'u' } : {}),
	});
	addSessionMessage(sessionId, msg);
	// A reviewed conversation with no persisted messages yet (e.g. after
	// rolling back the very first user message) is rebuilt with a
	// display-only `placeholder-*` bubble carrying the session input text.
	// The submitted message is the real start of the conversation: drop
	// the stand-in so the original input is never shown twice.
	updateSessionMessages(sessionId, (list) => {
		if (!list.some((m) => m.id.startsWith('placeholder-'))) return list;
		return list.filter((m) => !m.id.startsWith('placeholder-'));
	});
	try {
		const result = await invoke('process_transcript', {
			transcript: text,
			activeSessionId: activeId || null,
			attachments: hasAttachments ? attachments : null,
			voice,
		});
		if (result && result.SessionCreated && result.SessionCreated !== sessionId) {
			// This submission created the new session. Clear the fresh-start intent
			// ONLY if the fresh-start was already active when this submission was
			// dispatched — otherwise a 新对话 click that landed mid-flight must
			// not be cancelled, or the blank draft would be hijacked by this
			// (older) submission. Cleared BEFORE the store write so the page's
			// store→state follow effect (guarded by the intent) can adopt the
			// new session when appropriate.
			if (freshStartAtDispatch) {
				newSessionIntentStore.set(false);
				if (browser) localStorage.removeItem(NEW_ACTION_INTENT_KEY);
			}
			moveSessionMessages(sessionId, result.SessionCreated);
			activeSessionIdStore.set(result.SessionCreated);
		}
		return result;
	} catch (e) {
		updateSessionMessages(sessionId, (list) => list.filter((x) => x.id !== msg.id));
		throw e;
	}
}
