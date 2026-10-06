/**
 * Single source of truth for user-facing session/task terminology.
 *
 * The wire/runtime entity is `ToolRun`; the UI calls its two kinds
 * "后台任务" and "定时任务". A foreground row is the conversation itself,
 * not a foreground task.
 */

export type TaskKind = 'foreground' | 'background' | 'scheduled';

export const TASK_KIND_LABELS: Record<TaskKind, string> = {
	foreground: '会话',
	background: '后台任务',
	scheduled: '定时任务',
};

const TOOL_RUN_STATUS_LABELS: Record<string, string> = {
	waiting: '待执行',
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
};

/** Return the stable Chinese label for a task kind. */
export function toolRunKindLabel(kind: string | undefined): string {
	return (kind && TASK_KIND_LABELS[kind as TaskKind]) || '任务';
}

/** Return the stable Chinese label for a ToolRun status. */
export function toolRunStatusLabel(status: unknown): string {
	if (typeof status !== 'string' || !status.trim()) return '';
	return TOOL_RUN_STATUS_LABELS[status] || status;
}

/** Return the stable Chinese label for a scheduled ToolRun mode. */
export function scheduleModeLabel(mode: unknown): string {
	if (typeof mode !== 'string' || !mode.trim()) return '调用工具';
	return SCHEDULE_MODE_LABELS[mode] || mode;
}

/**
 * Resolve the title shown in the task workspace.
 *
 * User-provided title/body wins. A background ToolRun without that context
 * uses the same generic tool-call fallback as the chat card; a scheduled
 * ToolRun stays identifiable as a scheduled task.
 */
export function toolRunTitle(toolRun: {
	kind?: string;
	title?: unknown;
	body?: unknown;
	command?: unknown;
}): string {
	for (const candidate of [toolRun.title, toolRun.body]) {
		if (typeof candidate === 'string' && candidate.trim()) return candidate.trim();
	}
	return toolRun.kind === 'scheduled' ? TASK_KIND_LABELS.scheduled : '调用工具';
}
