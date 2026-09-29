import type { ActionPayload, ActionStatus } from './contracts/action.ts';
import { scheduleModeLabel, taskTitle } from './taskTerminology.ts';

/** Kind-specific fields retained by the shared action-card projection. */
export interface ActionCardDetails {
	command?: string;
	output?: string;
	error?: string;
	errorReason?: string;
	exitCode?: number;
	preview?: string;
	dueAt?: string;
	title?: string;
	body?: string;
	mode?: string;
}

/** Shared visible structure for background and scheduled action cards. */
export interface ActionCardProjection {
	id: string;
	kind: ActionPayload['kind'];
	status?: ActionStatus;
	sessionId?: string;
	title: string;
	searchText: string;
	statusLabel: string;
	tone: string;
	summary: string;
	context: string;
	timing: string;
	details: ActionCardDetails;
}

export interface ActionCardProjectionOptions {
	actionStatusLabel: (status: string) => string;
	sessionTitleFor: (action: ActionPayload) => string;
	actionDuration: (action: ActionPayload) => string;
	scheduledActionCountdown: (dueAt?: string) => string;
}

function scheduledStatusLabel(status?: ActionStatus): string {
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

function scheduledTone(status?: ActionStatus): string {
	if (status === 'waiting') return 'scheduled';
	if (status === 'running') return 'running';
	if (status === 'failed') return 'error';
	if (status === 'completed') return 'success';
	return 'neutral';
}

function backgroundTone(status?: ActionStatus): string {
	if (status === 'failed') return 'error';
	if (status === 'completed') return 'success';
	return status === 'running' ? 'running' : 'neutral';
}

function rowSummary(
	action: ActionPayload,
	title: string,
	status: ActionStatus | undefined,
	options: ActionCardProjectionOptions,
): string {
	const terminalDetail = status === 'failed' ? action.error : action.output;
	for (const candidate of [
		action.command,
		terminalDetail,
		action.preview,
		action.error,
		action.output,
		action.body,
	]) {
		if (
			typeof candidate === 'string' &&
			candidate.trim() &&
			candidate.trim().toLocaleLowerCase() !== title.trim().toLocaleLowerCase()
		) {
			return candidate.trim();
		}
	}
	if (action.kind === 'scheduled') {
		if (status === 'running') return `已触发 · ${scheduleModeLabel(action.mode)}`;
		const timing = options.scheduledActionCountdown(action.dueAt) || '时间未设置';
		return `将在${timing}执行 · ${scheduleModeLabel(action.mode)}`;
	}
	return '正在执行后台任务';
}

function rowTiming(
	action: ActionPayload,
	status: ActionStatus | undefined,
	options: ActionCardProjectionOptions,
): string {
	if (action.kind === 'background') {
		return options.actionDuration(action) || '耗时未知';
	}
	if (status === 'running') return options.actionDuration(action) || '执行中';
	return options.scheduledActionCountdown(action.dueAt) || '时间未设置';
}

/**
 * Project either action kind into the same card structure while keeping the
 * source fields that belong only to background or scheduled actions.
 */
export function projectActionCard(
	action: ActionPayload,
	options: ActionCardProjectionOptions,
): ActionCardProjection {
	const status = action.status ?? (action.kind === 'scheduled' ? 'waiting' : undefined);
	const title = taskTitle(action);
	const scheduled = action.kind === 'scheduled';
	const sessionTitle = options.sessionTitleFor(action);
	const context = scheduled ? scheduleModeLabel(action.mode) : sessionTitle || '无关联会话';
	const details: ActionCardDetails = scheduled
		? {
				dueAt: action.dueAt,
				title: action.title,
				body: action.body,
				mode: action.mode,
			}
		: {
				command: action.command,
				output: action.output,
				error: action.error,
				errorReason: action.errorReason,
				exitCode: action.exitCode,
				preview: action.preview,
			};

	return {
		id: action.id,
		kind: action.kind,
		status,
		sessionId: action.sessionId,
		title,
		searchText: scheduled ? scheduleModeLabel(action.mode) : sessionTitle || '后台任务',
		statusLabel: scheduled
			? scheduledStatusLabel(status)
			: options.actionStatusLabel(status || ''),
		tone: scheduled ? scheduledTone(status) : backgroundTone(status),
		summary: rowSummary(action, title, status, options),
		context,
		timing: rowTiming(action, status, options),
		details,
	};
}
