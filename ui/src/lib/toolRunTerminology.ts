import type { ToolRunKind } from './contracts/toolRun.ts';

/**
 * Single source of truth for user-facing ToolRun terminology.
 *
 * The wire/runtime entity is `ToolRun`; the UI calls its two kinds
 * "后台任务" and "定时任务". Foreground is an execution mode, while a session
 * is a conversation; neither is a ToolRun kind.
 */

export const TOOL_RUN_KIND_LABELS: Record<ToolRunKind, string> = {
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

/** Return the stable Chinese label for a ToolRun kind. */
export function toolRunKindLabel(kind: ToolRunKind | undefined): string {
	return (kind && TOOL_RUN_KIND_LABELS[kind]) || '任务';
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
 * Resolve the title shown in the ToolRun workspace.
 *
 * User-provided title/body wins. A background ToolRun without that context
 * uses the same generic tool-call fallback as the chat card; a scheduled
 * ToolRun stays identifiable as a scheduled task.
 */
export function toolRunTitle(toolRun: {
	kind?: ToolRunKind;
	title?: unknown;
	body?: unknown;
	command?: unknown;
}): string {
	for (const candidate of [toolRun.title, toolRun.body]) {
		if (typeof candidate === 'string' && candidate.trim()) return candidate.trim();
	}
	return toolRun.kind === 'scheduled' ? TOOL_RUN_KIND_LABELS.scheduled : '调用工具';
}
