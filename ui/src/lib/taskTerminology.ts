/**
 * Single source of truth for user-facing session/task terminology.
 *
 * The wire/runtime entity remains `action`; the UI calls its two kinds
 * "后台任务" and "定时任务". A foreground row is the conversation itself,
 * not a foreground task.
 */

export type TaskKind = 'foreground' | 'background' | 'scheduled';

export const TASK_KIND_LABELS: Record<TaskKind, string> = {
	foreground: '会话',
	background: '后台任务',
	scheduled: '定时任务',
};

const ACTION_STATUS_LABELS: Record<string, string> = {
	running: '运行中',
	completed: '已完成',
	failed: '失败',
	cancelled: '已取消',
	not_found: '未找到',
	idle: '未开始',
};

const SCHEDULE_MODE_LABELS: Record<string, string> = {
	tool: '调用工具',
	continue: '继续会话',
	// Kept for old/externally produced rows; current backend uses the notify tool
	// within `tool` mode rather than a separate mode.
	notify: '发送通知',
};

/** Return the stable Chinese label for a task kind. */
export function taskKindLabel(kind: string | undefined): string {
	return (kind && TASK_KIND_LABELS[kind as TaskKind]) || '任务';
}

/** Return the stable Chinese label for an action status. */
export function actionStatusLabel(status: unknown): string {
	if (typeof status !== 'string' || !status.trim()) return '';
	return ACTION_STATUS_LABELS[status] || status;
}

/** Return the stable Chinese label for a scheduled action mode. */
export function scheduleModeLabel(mode: unknown): string {
	if (typeof mode !== 'string' || !mode.trim()) return '调用工具';
	return SCHEDULE_MODE_LABELS[mode] || mode;
}

/**
 * Resolve the title shown in the task center.
 *
 * User-provided title/body wins. A background action without that context
 * uses the same generic tool-call fallback as the chat card; a scheduled
 * action stays identifiable as a scheduled task.
 */
export function taskTitle(action: {
	kind?: string;
	title?: unknown;
	body?: unknown;
	command?: unknown;
}): string {
	for (const candidate of [action.title, action.body]) {
		if (typeof candidate === 'string' && candidate.trim()) return candidate.trim();
	}
	return action.kind === 'scheduled' ? TASK_KIND_LABELS.scheduled : '调用工具';
}
