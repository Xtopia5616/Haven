import type { AgentCompactionPayload, AgentMediaPlanPayload } from './contracts/agent.ts';
import type { NotificationType } from './notificationStore.ts';
import { formatTokenCount } from './sessionUsage.ts';
import {
	mediaPlanNoticeLabel,
	mediaPlanProjectionLabel,
	mediaPlanStrategyLabel,
} from './mediaPlanPresentation.ts';

export interface AgentEventToast {
	message: string;
	type: NotificationType;
	durationMs: number;
}

export function formatAgentCompactionToast(payload: AgentCompactionPayload): AgentEventToast {
	const before = formatTokenCount(payload.tokensBefore);
	const after = formatTokenCount(payload.tokensAfter);
	return payload.degraded
		? {
				message: `上下文空间不足，已降级压缩：${before} → ${after} tokens；较早内容已省略`,
				type: 'warning',
				durationMs: 4000,
			}
		: {
				message: `上下文压缩：${before} → ${after} tokens`,
				type: 'info',
				durationMs: 2500,
			};
}

export function formatAgentMediaPlanToast(payload: AgentMediaPlanPayload): AgentEventToast {
	const projections = payload.projections.map(mediaPlanProjectionLabel);
	const reasons = payload.notices
		.map((notice) => mediaPlanNoticeLabel(notice.code))
		.filter((label, index, all) => all.indexOf(label) === index);
	const details = [
		projections.length > 0 ? `已使用 ${projections.join('、')}` : '没有发送兼容的附件表示',
		`策略：${mediaPlanStrategyLabel(payload.strategy)}`,
		...reasons.map((reason) => `原因：${reason}`),
	];
	return {
		message: `附件表示（${payload.role}）：${details.join('；')}`,
		type: payload.notices.length > 0 ? 'warning' : 'info',
		durationMs: 5000,
	};
}
