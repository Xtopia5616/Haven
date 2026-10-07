import type { ToolRunPayload } from './contracts/toolRun.ts';
import type { SessionMessage } from './sessionReducer.ts';
import { sourceToolRunIdFromObservation } from './streaming.ts';

/** Messages that describe agent work rather than user-facing transcript content. */
const MERGED_MESSAGE_TYPES = new Set(['thought', 'reasoning', 'tool']);

export interface SessionMessageContextMenuRequest {
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

export interface SessionTimelineMessageItem {
	kind: 'message';
	message: SessionMessage;
	index: number;
}

export interface SessionTimelineActivityItem {
	kind: 'activity';
	id: string;
	entries: Array<{ message: SessionMessage; index: number }>;
	streaming: boolean;
	toolCount: number;
	stepCount: number;
}

export interface SessionTimelineToolRunItem {
	kind: 'tool_run';
	id: string;
	toolRun: ToolRunPayload;
	awaitingBackgroundResult: boolean;
	awaitingBackgroundCount: number;
	showTerminalOutput: boolean;
}

export interface SessionTimelineToolRunWaitItem {
	kind: 'tool_run_wait';
	id: 'awaiting-background-result';
	awaitingBackgroundCount: number;
}

export type SessionTimelineItem =
	| SessionTimelineMessageItem
	| SessionTimelineActivityItem
	| SessionTimelineToolRunItem
	| SessionTimelineToolRunWaitItem;
export type SessionTranscriptItem = SessionTimelineMessageItem | SessionTimelineActivityItem;

/** Return whether a message can be folded into the surrounding work process. */
export function isMergedSessionMessage(message: SessionMessage): boolean {
	return MERGED_MESSAGE_TYPES.has(message.type || '');
}

/**
 * Group adjacent agent-work messages into compact, independently collapsible
 * timeline items. User-facing messages and actionable ask cards remain as
 * standalone entries so the session transcript order and required toolRuns stay
 * obvious.
 */
export function groupSessionMessages(messages: SessionMessage[]): SessionTranscriptItem[] {
	const items: SessionTranscriptItem[] = [];
	let activity: SessionTimelineActivityItem | null = null;
	let precedingBoundaryId = 'root';

	const flushActivity = () => {
		if (!activity) return;
		items.push(activity);
		activity = null;
	};

	messages.forEach((message, index) => {
		if (!isMergedSessionMessage(message)) {
			flushActivity();
			items.push({ kind: 'message', message, index });
			precedingBoundaryId = message.id;
			return;
		}

		if (!activity) {
			// The first entry can change while a live tool batch is reconciled:
			// for example, a late thought/reasoning block may be inserted before
			// the already-visible tool cards. Anchor the group to the surrounding
			// session boundary and step, rather than to that mutable first
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
 * Resolve the ToolRun identity already present in the tool observation. This
 * keeps timeline placement tied to the durable source result instead of an
 * event arrival timestamp or display text.
 */
export function sourceToolRunId(message: SessionMessage): string | null {
	if (typeof message.sourceToolRunId === 'string' && message.sourceToolRunId) {
		return message.sourceToolRunId;
	}
	if (typeof message.toolRunId === 'string' && message.toolRunId) return message.toolRunId;
	if (message.type !== 'tool') return null;
	return sourceToolRunIdFromObservation(message.toolName, message.content);
}

export interface SessionTimelineOptions {
	toolRuns?: ToolRunPayload[];
	awaitingBackground?: boolean;
	awaitingBackgroundCount?: number;
}

function compareToolRuns(left: ToolRunPayload, right: ToolRunPayload): number {
	const leftTime = left.startedAt || left.dueAt || '';
	const rightTime = right.startedAt || right.dueAt || '';
	return leftTime.localeCompare(rightTime) || left.id.localeCompare(right.id);
}

/** Resolve the same running toolRun used for the timeline wait indicator. */
export function firstWaitingBackgroundToolRunId(
	toolRuns: ToolRunPayload[],
	awaitingBackground: boolean,
): string | null {
	if (!awaitingBackground) return null;
	return (
		[...toolRuns].sort(compareToolRuns).find(
			(toolRun) => toolRun.kind === 'background' && toolRun.status === 'running',
		)?.id ?? null
	);
}

/**
 * Place scheduled ToolRun cards after their source work item. Background
 * ToolRuns with a visible source are projected into that tool call's result
 * card; only rows without a visible source get a standalone timeline item.
 * Keep live work at the end of the timeline, then let it return to its source
 * position once it finishes. Ownership comes from validated ToolRunEvent data.
 */
export function groupSessionTimeline(
	messages: SessionMessage[],
	{
		toolRuns = [],
		awaitingBackground = false,
		awaitingBackgroundCount = 0,
	}: SessionTimelineOptions = {},
): SessionTimelineItem[] {
	const transcriptItems = groupSessionMessages(messages);
	const orderedToolRuns = [...toolRuns].sort(compareToolRuns);
	const firstWaitingToolRunId =
		firstWaitingBackgroundToolRunId(orderedToolRuns, awaitingBackground) ?? undefined;
	const insertions = new Map<number, ToolRunPayload[]>();
	const trailing: ToolRunPayload[] = [];
	const runningSourceIndexes = new Set<number>();

	for (const toolRun of orderedToolRuns) {
		const stepIndex = toolRun.sourceStepId
			? messages.findIndex((message) => message.id === toolRun.sourceStepId)
			: -1;
		const sourceIndex =
			stepIndex >= 0
				? stepIndex
				: messages.findIndex(
						(message) =>
							message.type === 'tool' && sourceToolRunId(message) === toolRun.id,
					);
		if (sourceIndex < 0) {
			trailing.push(toolRun);
			continue;
		}
		if (toolRun.status === 'running') runningSourceIndexes.add(sourceIndex);
		const timelineIndex = transcriptItems.findIndex((item) =>
			item.kind === 'message'
				? item.index === sourceIndex
				: item.kind === 'activity' &&
					item.entries.some((entry) => entry.index === sourceIndex),
		);
		if (timelineIndex < 0) {
			trailing.push(toolRun);
			continue;
		}
		if (toolRun.kind === 'background') {
			continue;
		}
		const anchored = insertions.get(timelineIndex) || [];
		anchored.push(toolRun);
		insertions.set(timelineIndex, anchored);
	}

	const result: SessionTimelineItem[] = [];
	const activeItems: SessionTimelineItem[] = [];
	// A live item stays visible after transcript rows that arrived after its source.
	const isLiveTimelineItem = (item: SessionTimelineItem) =>
			(item.kind === 'activity' &&
				(item.streaming ||
					item.entries.some(({ index }) => runningSourceIndexes.has(index)))) ||
			(item.kind === 'message' && runningSourceIndexes.has(item.index)) ||
			(item.kind === 'tool_run' && item.toolRun.status === 'running');
	const appendTimelineItem = (
		item: SessionTimelineItem,
		keepWithLiveSource = false,
	) => {
		((keepWithLiveSource || isLiveTimelineItem(item)) ? activeItems : result).push(item);
	};
	const toolRunItem = (toolRun: ToolRunPayload): SessionTimelineToolRunItem => {
		const awaitingResult = toolRun.id === firstWaitingToolRunId;
		const terminalOutputAlreadyInTranscript =
			toolRun.kind === 'background' &&
			toolRun.status !== 'running' &&
			toolRun.status !== 'waiting' &&
			messages.some(
				(message) =>
					message.sourceToolRunId === toolRun.id &&
					message.toolRunId !== toolRun.id &&
					!message.streaming,
			);
		return {
			kind: 'tool_run',
			id: `tool-run-${toolRun.id}`,
			toolRun,
			awaitingBackgroundResult: awaitingResult,
			awaitingBackgroundCount: awaitingResult ? awaitingBackgroundCount : 0,
			showTerminalOutput: !terminalOutputAlreadyInTranscript,
		};
	};

	transcriptItems.forEach((item, index) => {
		const hasLiveSource = isLiveTimelineItem(item);
		appendTimelineItem(item);
		for (const toolRun of insertions.get(index) || []) {
			appendTimelineItem(toolRunItem(toolRun), hasLiveSource);
		}
	});
	for (const toolRun of trailing) appendTimelineItem(toolRunItem(toolRun));
	result.push(...activeItems);

	if (awaitingBackground && !firstWaitingToolRunId) {
		result.push({
			kind: 'tool_run_wait',
			id: 'awaiting-background-result',
			awaitingBackgroundCount,
		});
	}
	return result;
}
