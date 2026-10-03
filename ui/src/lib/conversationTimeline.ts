import type { AgentToolResultEnvelope } from './contracts/agent.ts';
import type { ActionPayload } from './contracts/action.ts';
import { sourceActionIdFromObservation } from './streaming.ts';

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
	attachments?: ConversationAttachment[];
	options?: string[];
	awaiting?: boolean;
	received?: boolean;
	resolved?: unknown;
	actionId?: string | null;
	sourceActionId?: string | null;
	outcome?: string | null;
	renderer?: string | null;
	result?: AgentToolResultEnvelope;
	showFallbackIntent?: boolean;
	stepNumber?: number | null;
	[key: string]: any;
}

export interface ConversationAttachment {
	media_type?: string;
	data?: string;
	filename?: string;
	path?: string;
}

export interface ConversationContextMenuRequest {
	x: number;
	y: number;
	messageId: string;
	stepNumber: number | null;
	role: string;
	content: string;
	type: string | null;
	selectedContent: string;
}

export type AskSelectionChangeHandler = (
	messageId: string,
	selected: string[] | null | undefined,
) => void;
export type AskMessageHandler = (messageId: string) => void;
export type AskSelectionGetter = (messageId: string) => string[];

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

export interface TimelineActionItem {
	kind: 'action';
	id: string;
	action: ActionPayload;
	awaitingBackgroundResult: boolean;
	awaitingBackgroundCount: number;
	showTerminalOutput: boolean;
}

export interface TimelineActionWaitItem {
	kind: 'action-wait';
	id: 'awaiting-background-result';
	awaitingBackgroundCount: number;
}

export type ConversationTimelineItem =
	TimelineMessageItem | TimelineActivityItem | TimelineActionItem | TimelineActionWaitItem;

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
	let precedingBoundaryId = 'root';

	const flushActivity = () => {
		if (!activity) return;
		items.push(activity);
		activity = null;
	};

	messages.forEach((message, index) => {
		if (!isMergedConversationMessage(message)) {
			flushActivity();
			items.push({ kind: 'message', message, index });
			precedingBoundaryId = message.id;
			return;
		}

		if (!activity) {
			// The first entry can change while a live tool batch is reconciled:
			// for example, a late thought/reasoning block may be inserted before
			// the already-visible tool cards. Anchor the group to the surrounding
			// conversation boundary and step, rather than to that mutable first
			// entry, so Svelte keeps the disclosure instances and their local open
			// state alive.
			const stepAnchor =
				message.stepNumber == null ? `message-${message.id}` : `step-${message.stepNumber}`;
			activity = {
				kind: 'activity',
				id: `activity-${precedingBoundaryId}-${stepAnchor}`,
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

/**
 * Resolve the Action identity already present in the tool observation. This
 * keeps timeline placement tied to the durable source result instead of an
 * event arrival timestamp or display text.
 */
export function sourceActionId(message: ConversationMessage): string | null {
	if (typeof message.sourceActionId === 'string' && message.sourceActionId) {
		return message.sourceActionId;
	}
	if (typeof message.actionId === 'string' && message.actionId) return message.actionId;
	if (message.type !== 'tool') return null;
	return sourceActionIdFromObservation(message.toolName, message.content);
}

export interface ConversationTimelineOptions {
	actions?: ActionPayload[];
	awaitingBackground?: boolean;
	awaitingBackgroundCount?: number;
}

/**
 * Place Action cards after the work item identified by source_step_id, falling
 * back to the Action ID in the tool observation for rows without that stable
 * step anchor. Rows without a visible source remain at the end of their owning
 * session timeline; ownership comes from the validated ActionEvent session_id.
 */
export function groupConversationTimeline(
	messages: ConversationMessage[],
	{
		actions = [],
		awaitingBackground = false,
		awaitingBackgroundCount = 0,
	}: ConversationTimelineOptions = {},
): ConversationTimelineItem[] {
	const transcriptItems = groupConversationMessages(messages);
	const orderedActions = [...actions].sort((left, right) => {
		const leftTime = left.startedAt || left.dueAt || '';
		const rightTime = right.startedAt || right.dueAt || '';
		return leftTime.localeCompare(rightTime) || left.id.localeCompare(right.id);
	});
	const firstWaitingActionId = awaitingBackground
		? orderedActions.find(
				(action) => action.kind === 'background' && action.status === 'running',
			)?.id
		: undefined;
	const insertions = new Map<number, ActionPayload[]>();
	const trailing: ActionPayload[] = [];

	for (const action of orderedActions) {
		const stepIndex = action.sourceStepId
			? messages.findIndex((message) => message.id === action.sourceStepId)
			: -1;
		const sourceIndex =
			stepIndex >= 0
				? stepIndex
				: messages.findIndex((message) => sourceActionId(message) === action.id);
		if (sourceIndex < 0) {
			trailing.push(action);
			continue;
		}
		const timelineIndex = transcriptItems.findIndex((item) =>
			item.kind === 'message'
				? item.index === sourceIndex
				: item.kind === 'activity' &&
					item.entries.some((entry) => entry.index === sourceIndex),
		);
		if (timelineIndex < 0) {
			trailing.push(action);
			continue;
		}
		const anchored = insertions.get(timelineIndex) || [];
		anchored.push(action);
		insertions.set(timelineIndex, anchored);
	}

	const result: ConversationTimelineItem[] = [];
	const addAction = (action: ActionPayload) => {
		const awaitingResult = action.id === firstWaitingActionId;
		const terminalOutputAlreadyInTranscript =
			action.kind === 'background' &&
			action.status !== 'running' &&
			action.status !== 'waiting' &&
			messages.some(
				(message) =>
					message.sourceActionId === action.id &&
					message.actionId !== action.id &&
					!message.streaming,
			);
		result.push({
			kind: 'action',
			id: `action-${action.id}`,
			action,
			awaitingBackgroundResult: awaitingResult,
			awaitingBackgroundCount: awaitingResult ? awaitingBackgroundCount : 0,
			showTerminalOutput: !terminalOutputAlreadyInTranscript,
		});
	};

	transcriptItems.forEach((item, index) => {
		result.push(item);
		for (const action of insertions.get(index) || []) addAction(action);
	});
	for (const action of trailing) addAction(action);

	if (awaitingBackground && !firstWaitingActionId) {
		result.push({
			kind: 'action-wait',
			id: 'awaiting-background-result',
			awaitingBackgroundCount,
		});
	}
	return result;
}
