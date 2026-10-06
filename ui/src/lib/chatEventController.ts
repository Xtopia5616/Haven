import { createChatAgentEventHandlers } from './chatAgentEventHandlers.ts';
import { createChatSessionEventHandler } from './chatSessionEventHandlers.ts';
import { createChatUsageEventHandlers } from './chatUsageEventHandlers.ts';
import {
	agentEventListeners,
	appEventListeners,
	registerListeners,
	sessionEventListeners,
} from './events.ts';
import type { ChatSessionEventContext } from './chatSessionEventHandlers.ts';
import type { StreamEventAggregator } from './streamAggregator.ts';

type ChatEventListenerMap = ReturnType<typeof sessionEventListeners>;

export interface ChatEventRegistration {
	ready: Promise<void>;
	dispose: () => void;
}

export type ChatEventRegistrationPort = (
	listeners: ChatEventListenerMap,
	options: { tag: string },
) => ChatEventRegistration;

/** The page callbacks required to compose its event handlers. */
export interface ChatEventControllerDependencies extends ChatSessionEventContext {
	chunkHandler: StreamEventAggregator['chunkHandler'];
	setHotkeyBinding: (binding: string) => void;
	getSkipNextDefaultModelRefresh: () => boolean;
	clearSkipNextDefaultModelRefresh: () => void;
	refreshDefaultModelFromBackend: () => void;
}

export interface ChatEventController {
	/** Register every chat page channel once and resolve when listeners are ready. */
	register: () => Promise<void>;
	/** Dispose registrations once; the registration port owns late subscriptions. */
	dispose: () => void;
}

/**
 * Own chat-page listener composition and its registration lifetime.
 * Wire mapping and the shared Tauri registration primitive remain in events.ts.
 */
export function createChatEventController(
	dependencies: ChatEventControllerDependencies,
	registerPort: ChatEventRegistrationPort = registerListeners,
): ChatEventController {
	let registration: ChatEventRegistration | null = null;
	let ready: Promise<void> | null = null;
	let disposed = false;

	function register(): Promise<void> {
		if (disposed) return Promise.resolve();
		if (ready) return ready;

		const listenerMap: ChatEventListenerMap = {
			...sessionEventListeners(createChatSessionEventHandler(dependencies)),
			...appEventListeners({
				'hotkey:rebind': (event) => {
					const binding = event.payload.newBinding;
					if (binding) dependencies.setHotkeyBinding(binding);
				},
				// Settings save / model switch rebuilds the router. Keep-alive leaves
				// this page mounted, so refresh the chat model unless this page just
				// initiated the model change itself.
				'llm:config_changed': () => {
					if (dependencies.getSkipNextDefaultModelRefresh()) {
						dependencies.clearSkipNextDefaultModelRefresh();
						return;
					}
					dependencies.refreshDefaultModelFromBackend();
				},
			}),
			...agentEventListeners(
				createChatAgentEventHandlers({
					chunkHandler: dependencies.chunkHandler,
					flushChunksNow: dependencies.flushChunksNow,
					dispatchSession: dependencies.dispatchSession,
				}),
			),
			...agentEventListeners(
				createChatUsageEventHandlers({ dispatchSession: dependencies.dispatchSession }),
			),
		};

		registration = registerPort(listenerMap, { tag: '+page' });
		ready = registration.ready;
		return ready;
	}

	function dispose(): void {
		if (disposed) return;
		disposed = true;
		registration?.dispose();
	}

	return { register, dispose };
}
