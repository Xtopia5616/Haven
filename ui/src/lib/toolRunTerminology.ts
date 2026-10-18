import type { ToolRunKind } from './contracts/toolRun.ts';

const TOOL_RUN_KIND_LABELS: Record<ToolRunKind, string> = {
	background: '后台任务',
};

const TOOL_RUN_STATUS_LABELS: Record<string, string> = {
	waiting: '排队中',
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
	new_session: '新建会话',
};

export function toolRunKindLabel(kind: ToolRunKind | undefined): string {
	return (kind && TOOL_RUN_KIND_LABELS[kind]) || '后台任务';
}

export function toolRunStatusLabel(status: unknown): string {
	if (typeof status !== 'string' || !status.trim()) return '';
	return TOOL_RUN_STATUS_LABELS[status] || status;
}

export function scheduleModeLabel(mode: unknown): string {
	if (typeof mode !== 'string' || !mode.trim()) return '调用工具';
	return SCHEDULE_MODE_LABELS[mode] || mode;
}

export function toolRunTitle(toolRun: { toolName?: unknown }): string {
	return typeof toolRun.toolName === 'string' && toolRun.toolName.trim()
		? `后台调用 · ${toolRun.toolName}`
		: '后台工具调用';
}
