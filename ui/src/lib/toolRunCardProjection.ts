import type { ToolRunPayload, ToolRunStatus } from './contracts/toolRun.ts';
import { toolRunTitle } from './toolRunTerminology.ts';
import type { StatusTone } from './statusColors.ts';

export type ToolRunStatusBadgeTone = Extract<
	StatusTone,
	'neutral' | 'info' | 'success' | 'warning' | 'error'
>;
export type ToolRunCardTone = 'running' | 'error' | 'success' | 'neutral';

export interface ToolRunCardDetails {
	output?: string;
	error?: string;
	preview?: string;
}

export interface ToolRunCardProjection {
	toolRunId: string;
	kind: ToolRunPayload['kind'];
	toolName: string;
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
}

export function toolRunCardBadgeTone(tone: ToolRunCardTone): ToolRunStatusBadgeTone {
	if (tone === 'error') return 'error';
	if (tone === 'success') return 'success';
	if (tone === 'running') return 'info';
	return 'neutral';
}

function cardTone(status?: ToolRunStatus): ToolRunCardTone {
	if (status === 'failed') return 'error';
	if (status === 'completed') return 'success';
	return status === 'running' ? 'running' : 'neutral';
}

export function projectToolRunCard(
	toolRun: ToolRunPayload,
	options: ToolRunCardProjectionOptions,
): ToolRunCardProjection {
	const title = toolRunTitle(toolRun);
	const status = toolRun.status;
	const sessionTitle = options.sessionTitleFor(toolRun);
	const terminalDetail = status === 'failed' ? toolRun.error : toolRun.output;
	const summary =
		[terminalDetail, toolRun.preview, toolRun.error, toolRun.output].find(
			(value) =>
				typeof value === 'string' &&
				value.trim() &&
				value.trim().toLocaleLowerCase() !== title.trim().toLocaleLowerCase(),
		) || '正在执行后台任务';
	return {
		toolRunId: toolRun.toolRunId,
		kind: toolRun.kind,
		toolName: toolRun.toolName,
		status,
		sessionId: toolRun.sessionId,
		title,
		searchText: `${sessionTitle} ${toolRun.toolName}`.trim() || '后台任务',
		statusLabel: status ? options.toolRunStatusLabel(status) : '',
		tone: cardTone(status),
		summary: summary.trim(),
		context: sessionTitle || '无关联会话',
		timing: options.toolRunDuration(toolRun) || '耗时未知',
		details: {
			output: toolRun.output,
			error: toolRun.error,
			preview: toolRun.preview,
		},
	};
}
