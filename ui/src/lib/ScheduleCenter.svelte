<script lang="ts">
	import { fade } from 'svelte/transition';
	import AsyncState from '$lib/AsyncState.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import type { SchedulePlanPayload } from '$lib/contracts/schedule.ts';
	import { scheduleModeLabel } from '$lib/toolRunTerminology.ts';

	interface Props {
		schedules?: SchedulePlanPayload[];
		loading?: boolean;
		failed?: boolean;
		sessionTitleFor?: (sessionId: string) => string;
		onRefresh?: () => void;
		onCancel?: (scheduleId: string, sessionId: string) => void;
		onOpenSession?: (sessionId: string) => void;
	}

	let {
		schedules = [],
		loading = false,
		failed = false,
		sessionTitleFor = () => '',
		onRefresh = () => {},
		onCancel = () => {},
		onOpenSession = () => {},
	}: Props = $props();

	function scheduleTime(plan: SchedulePlanPayload): string {
		const raw = plan.nextDueAt || plan.dueAt;
		if (!raw) return plan.watchToolRunId ? '等待后台任务完成' : '时间未设置';
		const time = new Date(raw);
		return Number.isNaN(time.getTime())
			? raw
			: new Intl.DateTimeFormat('zh-CN', {
					month: 'numeric',
					day: 'numeric',
					hour: '2-digit',
					minute: '2-digit',
				}).format(time);
	}
</script>

<section class="schedule-center" aria-label="定时任务计划">
	<header class="schedule-header">
		<div>
			<h2>定时任务</h2>
			<p>由 Schedule 计划触发，可继续会话、调用工具或创建新会话。</p>
		</div>
		<MaterialButton variant="text" label="刷新计划" onclick={onRefresh} disabled={loading} />
	</header>

	{#if loading && schedules.length === 0}
		<AsyncState state="loading" title="正在加载定时任务" message="正在读取已启用的计划。" />
	{:else if failed && schedules.length === 0}
		<AsyncState
			state="error"
			title="定时任务加载失败"
			message="检查应用连接后重试。"
			actionLabel="重试"
			onAction={onRefresh}
		/>
	{:else if schedules.length === 0}
		<AsyncState title="暂无定时任务" message="安排 Schedule 计划后，会显示下一次触发时间。" />
	{:else}
		<ul class="schedule-list motion-list">
			{#each schedules as plan (plan.scheduleId)}
				<li class="schedule-card motion-list-item" out:fade={{ duration: 140 }}>
					<div class="schedule-copy">
						<div class="schedule-title-row">
							<h3>{plan.title}</h3>
							<span class="schedule-mode">{scheduleModeLabel(plan.mode)}</span>
						</div>
						<p>{plan.body}</p>
						<div class="schedule-meta">
							<span>{scheduleTime(plan)}</span>
							<span>{sessionTitleFor(plan.ownerSessionId) || '来源会话'}</span>
							{#if plan.intervalSeconds}
								<span>每 {Math.round(plan.intervalSeconds / 60)} 分钟重复</span>
							{/if}
						</div>
					</div>
					<div class="schedule-actions">
						<MaterialButton
							variant="outlined"
							label="打开会话"
							onclick={() => onOpenSession(plan.ownerSessionId)}
						/>
						<MaterialButton
							variant="danger"
							label="取消计划"
							onclick={() => onCancel(plan.scheduleId, plan.ownerSessionId)}
						/>
					</div>
				</li>
			{/each}
		</ul>
	{/if}
</section>

<style>
	.schedule-center {
		width: 100%;
		min-width: 0;
		margin-top: var(--md-sys-space-xl);
		container-type: inline-size;
	}
	.schedule-header,
	.schedule-title-row,
	.schedule-meta,
	.schedule-actions {
		display: flex;
		align-items: center;
		gap: 0.75rem;
	}
	.schedule-header,
	.schedule-title-row {
		justify-content: space-between;
	}
	.schedule-header h2,
	.schedule-title-row h3,
	.schedule-header p,
	.schedule-copy p {
		margin: 0;
	}
	.schedule-header p,
	.schedule-copy p,
	.schedule-meta {
		color: var(--md-sys-color-on-surface-variant);
	}
	.schedule-header p {
		margin-top: 0.25rem;
	}
	.schedule-list {
		display: grid;
		gap: 0.75rem;
		padding: 0;
		margin: 0.75rem 0 0;
		list-style: none;
	}
	.schedule-card {
		display: flex;
		justify-content: space-between;
		align-items: center;
		gap: 1rem;
		padding: 1rem;
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: 0.75rem;
		background: var(--md-sys-color-surface-container-low);
	}
	.schedule-mode {
		padding: 0.15rem 0.5rem;
		border-radius: 999px;
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
		font-size: 0.8rem;
		white-space: nowrap;
	}
	.schedule-meta {
		display: flex;
		flex-wrap: wrap;
		margin-top: 0.45rem;
		font-size: 0.82rem;
	}
	.schedule-meta span + span::before {
		content: '·';
		margin-right: 0.75rem;
	}
	.schedule-actions {
		flex-shrink: 0;
	}
	@container (max-width: 760px) {
		.schedule-header {
			align-items: flex-start;
		}
		.schedule-card {
			align-items: flex-start;
			flex-direction: column;
			padding: var(--md-sys-space-md);
		}
		.schedule-actions {
			width: 100%;
			justify-content: flex-end;
			flex-wrap: wrap;
		}
	}
	@container (max-width: 420px) {
		.schedule-title-row {
			align-items: flex-start;
			flex-wrap: wrap;
		}
		.schedule-meta {
			align-items: flex-start;
			flex-direction: column;
			gap: var(--md-sys-space-2xs);
		}
		.schedule-meta span + span::before {
			content: none;
		}
		.schedule-actions {
			align-items: stretch;
			flex-direction: column;
		}
	}
	@media screen and (orientation: landscape) and (max-height: 560px) {
		.schedule-center {
			margin-top: var(--md-sys-space-md);
		}
		.schedule-list {
			gap: var(--md-sys-space-sm);
			margin-top: var(--md-sys-space-sm);
		}
		.schedule-card {
			padding: var(--md-sys-space-sm);
		}
	}
</style>
