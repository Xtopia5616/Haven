import type { AgentToolResultEnvelope } from './contracts/agent.ts';

/** Messages that describe agent work rather than user-facing conversation. */
const MERGED_MESSAGE_TYPES = new Set(['thought', 'reasoning', 'tool']);

export interface ConversationMessage {
	id: string;
	role?: string;
	content?: string;
	type?: string | null;
	streaming?: boolean;
	voice?: boolean;
	time?: string | null;
	toolName?: string | null;
	toolArgs?: unknown;
	attachments?: unknown[];
	options?: string[];
	awaiting?: boolean;
	received?: boolean;
	resolved?: unknown;
	actionId?: string | null;
	outcome?: string | null;
	renderer?: string | null;
	result?: AgentToolResultEnvelope;
	showFallbackIntent?: boolean;
	stepNumber?: number | null;
	[key: string]: any;
}

export interface TimelineMessageItem {
	kind: 'message';
	message: ConversationMessage;
	index: number;
}

export interface TimelineActivityItem {
	kind: 'activity';
	id: string;
	entries: Array<{ message: ConversationMessage; index: number }>;
	streaming: boolean;
	toolCount: number;
	stepCount: number;
}

export type ConversationTimelineItem = TimelineMessageItem | TimelineActivityItem;

/** Return whether a message can be folded into the surrounding work process. */
export function isMergedConversationMessage(message: ConversationMessage): boolean {
	return MERGED_MESSAGE_TYPES.has(message.type || '');
}

/**
 * Group adjacent agent-work messages into compact, independently collapsible
 * timeline items. User-facing messages and actionable ask cards remain as
 * standalone entries so the conversation order and required actions stay
 * obvious.
 */
export function groupConversationMessages(
	messages: ConversationMessage[],
): ConversationTimelineItem[] {
	const items: ConversationTimelineItem[] = [];
	let activity: TimelineActivityItem | null = null;

	const flushActivity = () => {
		if (!activity) return;
		items.push(activity);
		activity = null;
	};

	messages.forEach((message, index) => {
		if (!isMergedConversationMessage(message)) {
			flushActivity();
			items.push({ kind: 'message', message, index });
			return;
		}

		if (!activity) {
			activity = {
				kind: 'activity',
				id: `activity-${message.id}`,
				entries: [],
				streaming: false,
				toolCount: 0,
				stepCount: 0,
			};
		}

		activity.entries.push({ message, index });
		activity.streaming ||= message.streaming === true;
		if (message.type === 'tool') activity.toolCount += 1;
	});

	flushActivity();

	for (const item of items) {
		if (item.kind !== 'activity') continue;
		const stepKeys = new Set(
			item.entries.map(({ message }, index) =>
				message.stepNumber == null ? `entry-${index}` : `step-${message.stepNumber}`,
			),
		);
		item.stepCount = stepKeys.size;
	}

	return items;
}
