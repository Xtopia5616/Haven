import type { ToolRunPayload, ToolRunStatus } from './contracts/toolRun.ts';
import { scheduleModeLabel, toolRunTitle } from './toolRunTerminology.ts';
import type { StatusTone } from './statusColors.ts';

export type ToolRunStatusBadgeTone = Extract<
	StatusTone,
	'neutral' | 'info' | 'success' | 'warning' | 'error'
>;

export type ToolRunCardTone = 'scheduled' | 'running' | 'error' | 'success' | 'neutral';

/** Kind-specific fields retained by the shared ToolRun card projection. */
export interface ToolRunCardDetails {
	command?: string;
	output?: string;
	error?: string;
	preview?: string;
	body?: string;
}

/** Shared visible structure for background and scheduled ToolRun cards. */
export interface ToolRunCardProjection {
	toolRunId: string;
	kind: ToolRunPayload['kind'];
	status?: ToolRunStatus;
	sessionId?: string;
	title: string;
	searchText: string;
	statusLabel: string;
	tone: ToolRunCardTone;
	summary: string;
	context: string;
	timing: string;
	details: ToolRunCardDetails;
}

export interface ToolRunCardProjectionOptions {
	toolRunStatusLabel: (status: ToolRunStatus | undefined) => string;
	sessionTitleFor: (toolRun: Pick<ToolRunPayload, 'sessionId'>) => string;
	toolRunDuration: (toolRun: ToolRunPayload) => string;
	scheduledToolRunCountdown: (dueAt?: string) => string;
}

/** Map run-card state to the shared semantic badge palette. */
export function toolRunCardBadgeTone(tone: ToolRunCardTone): ToolRunStatusBadgeTone {
	if (tone === 'error') return 'error';
	if (tone === 'success') return 'success';
	if (tone === 'running') return 'info';
	if (tone === 'scheduled') return 'warning';
	return 'neutral';
}

function scheduledStatusLabel(status?: ToolRunStatus): string {
	switch (status) {
		case 'running':
			return '执行中';
		case 'completed':
			return '已完成';
		case 'failed':
			return '失败';
		case 'cancelled':
			return '已取消';
		default:
			return '待执行';
	}
}

function scheduledTone(status?: ToolRunStatus): ToolRunCardTone {
	if (status === 'waiting') return 'scheduled';
	if (status === 'running') return 'running';
	if (status === 'failed') return 'error';
	if (status === 'completed') return 'success';
	return 'neutral';
}

function backgroundTone(status?: ToolRunStatus): ToolRunCardTone {
	if (status === 'failed') return 'error';
	if (status === 'completed') return 'success';
	return status === 'running' ? 'running' : 'neutral';
}

function rowSummary(
	toolRun: ToolRunPayload,
	title: string,
	status: ToolRunStatus | undefined,
	options: ToolRunCardProjectionOptions,
): string {
	const terminalDetail = status === 'failed' ? toolRun.error : toolRun.output;
	for (const candidate of [
		toolRun.command,
		terminalDetail,
		toolRun.preview,
		toolRun.error,
		toolRun.output,
		toolRun.body,
	]) {
		if (
			typeof candidate === 'string' &&
			candidate.trim() &&
			candidate.trim().toLocaleLowerCase() !== title.trim().toLocaleLowerCase()
		) {
			return candidate.trim();
		}
	}
	if (toolRun.kind === 'scheduled') {
		if (status === 'running') return `已触发 · ${scheduleModeLabel(toolRun.mode)}`;
		const timing = options.scheduledToolRunCountdown(toolRun.dueAt) || '时间未设置';
		return `将在${timing}执行 · ${scheduleModeLabel(toolRun.mode)}`;
	}
	return '正在执行后台任务';
}

function rowTiming(
	toolRun: ToolRunPayload,
	status: ToolRunStatus | undefined,
	options: ToolRunCardProjectionOptions,
): string {
	if (toolRun.kind === 'background') {
		return options.toolRunDuration(toolRun) || '耗时未知';
	}
	if (status === 'running') return options.toolRunDuration(toolRun) || '执行中';
	return options.scheduledToolRunCountdown(toolRun.dueAt) || '时间未设置';
}

/**
 * Project either ToolRun kind into the same card structure while keeping the
 * fields that belong only to background or scheduled runs.
 */
export function projectToolRunCard(
	toolRun: ToolRunPayload,
	options: ToolRunCardProjectionOptions,
): ToolRunCardProjection {
	const status = toolRun.status ?? (toolRun.kind === 'scheduled' ? 'waiting' : undefined);
	const title = toolRunTitle(toolRun);
	const scheduled = toolRun.kind === 'scheduled';
	const sessionTitle = options.sessionTitleFor(toolRun);
	const context = scheduled ? scheduleModeLabel(toolRun.mode) : sessionTitle || '无关联会话';
	const details: ToolRunCardDetails = scheduled
		? {
				body: toolRun.body,
			}
		: {
				command: toolRun.command,
				output: toolRun.output,
				error: toolRun.error,
				preview: toolRun.preview,
			};

	return {
		toolRunId: toolRun.toolRunId,
		kind: toolRun.kind,
		status,
		sessionId: toolRun.sessionId,
		title,
		searchText: scheduled ? scheduleModeLabel(toolRun.mode) : sessionTitle || '后台任务',
		statusLabel: scheduled
			? scheduledStatusLabel(status)
			: status
				? options.toolRunStatusLabel(status)
				: '',
		tone: scheduled ? scheduledTone(status) : backgroundTone(status),
		summary: rowSummary(toolRun, title, status, options),
		context,
		timing: rowTiming(toolRun, status, options),
		details,
	};
}
