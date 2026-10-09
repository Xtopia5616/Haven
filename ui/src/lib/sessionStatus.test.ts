import { describe, it, expect } from 'vitest';
import {
	SESSION_STATUS_VALUES,
	SESSION_WAITING_REASON_VALUES,
} from './contracts/generatedCommands.ts';
import {
	isBusyStatus,
	isPausedStatus,
	sessionHistoryStatusLabel,
	sessionStatusLabel,
	sessionWaitingReason,
	statusColor,
	statusVariant,
	isErrorStatus,
	waitingReasonLabel,
} from './sessionStatus.ts';

describe('SESSION_STATUS_VALUES', () => {
	it('covers the canonical backend session statuses', () => {
		expect(SESSION_STATUS_VALUES).toEqual([
			'pending',
			'running',
			'paused',
			'completed',
			'error',
		]);
	});
});

describe('isPausedStatus', () => {
	it('treats the generic paused status as paused', () => {
		expect(isPausedStatus('paused')).toBe(true);
		expect(isPausedStatus('pending')).toBe(false);
		expect(isPausedStatus(undefined)).toBe(false);
	});
});

describe('session waiting reasons', () => {
	it('keeps the backend vocabulary and labels stable', () => {
		expect(SESSION_WAITING_REASON_VALUES).toEqual([
			'user_input',
			'user_interrupt',
			'ask',
			'confirmation',
			'scheduled_confirmation',
			'background_task',
			'scheduled_task',
			'step_budget',
			'end_incomplete',
		]);
		expect(waitingReasonLabel('user_input')).toBe('等待操作');
		expect(waitingReasonLabel('ask')).toBe('等待操作');
		expect(waitingReasonLabel('confirmation')).toBe('等待操作');
		expect(waitingReasonLabel('scheduled_confirmation')).toBe('等待操作');
		expect(waitingReasonLabel('step_budget')).toBe('等待操作');
		expect(waitingReasonLabel('background_task')).toBe('等待任务');
		expect(waitingReasonLabel('end_incomplete')).toBe('结束未完成，可重试');
		expect(waitingReasonLabel('unknown')).toBeNull();
	});

	it('reads the normalized list payload field', () => {
		expect(sessionWaitingReason({ waitingReason: 'confirmation' })).toBe('confirmation');
		expect(sessionWaitingReason(undefined)).toBeNull();
	});
});

describe('isBusyStatus', () => {
	it('treats pending and running as busy', () => {
		expect(isBusyStatus('pending')).toBe(true);
		expect(isBusyStatus('running')).toBe(true);
		expect(isBusyStatus('paused')).toBe(false);
		expect(isBusyStatus('completed')).toBe(false);
		expect(isBusyStatus(undefined)).toBe(false);
	});
});

describe('statusColor', () => {
	it('maps every status to a semantic theme color token', () => {
		expect(statusColor('pending')).toBe('var(--md-sys-color-outline)');
		expect(statusColor('running')).toBe('var(--md-sys-color-primary)');
		expect(statusColor('paused')).toBe('var(--md-sys-color-warning)');
		expect(statusColor('completed')).toBe('var(--md-sys-color-success)');
		expect(statusColor('error')).toBe('var(--md-sys-color-error)');
	});

	it('falls back to the neutral theme color for unknown statuses', () => {
		expect(statusColor('paused_pending')).toBe('var(--md-sys-color-outline)');
		expect(statusColor('')).toBe('var(--md-sys-color-outline)');
		expect(statusColor(undefined as any)).toBe('var(--md-sys-color-outline)');
	});
});

describe('statusVariant', () => {
	it('maps every status to its MaterialBadge variant', () => {
		expect(statusVariant('pending')).toBe('default');
		expect(statusVariant('running')).toBe('primary');
		expect(statusVariant('paused')).toBe('warning');
		expect(statusVariant('completed')).toBe('success');
		expect(statusVariant('error')).toBe('error');
	});

	it('falls back to default for unknown statuses', () => {
		expect(statusVariant('paused_pending')).toBe('default');
		expect(statusVariant(undefined as any)).toBe('default');
	});
});

describe('isErrorStatus', () => {
	it('keeps terminal failures distinct from paused sessions', () => {
		expect(isErrorStatus('error')).toBe(true);
		expect(isErrorStatus('paused')).toBe(false);
		expect(isErrorStatus('completed')).toBe(false);
	});
});

describe('sessionStatusLabel', () => {
	it('projects the current session state to its single display label', () => {
		expect(sessionStatusLabel(null)).toBe('空闲');
		expect(sessionStatusLabel({ status: 'pending' })).toBe('排队中');
		expect(sessionStatusLabel({ status: 'running' })).toBe('运行中');
		expect(sessionStatusLabel({ status: 'paused' })).toBe('已暂停');
		expect(sessionStatusLabel({ status: 'paused', waitingReason: 'background_task' })).toBe(
			'等待任务',
		);
		expect(sessionStatusLabel({ status: 'completed' })).toBe('空闲');
		expect(sessionStatusLabel({ status: 'error' })).toBe('错误');
	});

	it('keeps waiting reasons ahead of the generic paused label', () => {
		expect(sessionStatusLabel({ status: 'paused', waitingReason: 'scheduled_task' })).toBe(
			'等待定时任务',
		);
		expect(sessionStatusLabel({ status: 'paused', waitingReason: 'unknown' })).toBe('已暂停');
	});
});

describe('sessionHistoryStatusLabel', () => {
	it('names persisted history states with an explicit terminal label', () => {
		expect(sessionHistoryStatusLabel('pending')).toBe('排队中');
		expect(sessionHistoryStatusLabel('running')).toBe('运行中');
		expect(sessionHistoryStatusLabel('paused')).toBe('已暂停');
		expect(sessionHistoryStatusLabel('completed')).toBe('已完成');
		expect(sessionHistoryStatusLabel('error')).toBe('错误');
	});
});
