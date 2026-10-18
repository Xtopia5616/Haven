<script lang="ts">
	import { onMount } from 'svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import AsyncState from '$lib/AsyncState.svelte';
	import { addNotification } from '$lib/notificationStore.ts';
	import { reportError } from '$lib/errorHandling.ts';
	import { listRuntimeSessions } from '$lib/sessionCommands.ts';
	import type { RuntimeSessionListResponse } from '$lib/contracts/sessionHistory.ts';
	import {
		createWorkflowSchedule,
		deleteWorkflowDefinition,
		deleteWorkflowSchedule,
		listWorkflowSchedules,
		listWorkflowDefinitions,
		runWorkflowDefinition,
		saveWorkflowDefinition,
		setWorkflowScheduleEnabled,
		type WorkflowDefinitionDto,
		type WorkflowScheduleDto,
	} from '$lib/workflowCommands.ts';

	let workflows = $state<WorkflowDefinitionDto[]>([]);
	let selectedId = $state<string | null>(null);
	let name = $state('');
	let description = $state('');
	let definitionText = $state('{\n  "steps": []\n}');
	let loading = $state(true);
	let saving = $state(false);
	let running = $state(false);
	let scheduling = $state(false);
	let scheduleBusyId = $state<string | null>(null);
	let runtimeSessions = $state<RuntimeSessionListResponse['sessions']>([]);
	let schedules = $state<WorkflowScheduleDto[]>([]);
	let sessionId = $state('');
	let intervalMinutes = $state(1440);
	let error = $state<string | null>(null);

	const selected = $derived(workflows.find((workflow) => workflow.workflowId === selectedId) ?? null);
	const hasUnsavedChanges = $derived(
		selected
			? name !== selected.name ||
				description !== selected.description ||
				definitionText !== JSON.stringify(selected.definition, null, 2)
			: Boolean(name.trim() || description.trim() || definitionText !== '{\n  "steps": []\n}'),
	);

	onMount(() => {
		void refresh();
		void refreshRuntimeSessions();
		void refreshSchedules();
	});

	async function refreshRuntimeSessions() {
		try {
			runtimeSessions = (await listRuntimeSessions()).sessions;
			if (!runtimeSessions.some((session) => session.sessionId === sessionId)) {
				sessionId = runtimeSessions[0]?.sessionId ?? '';
			}
		} catch (cause) {
			reportError(cause, { context: 'WorkflowLibrary', message: '读取运行中会话失败', log: false });
		}
	}

	async function refreshSchedules() {
		try {
			schedules = await listWorkflowSchedules();
		} catch (cause) {
			reportError(cause, { context: 'WorkflowLibrary', message: '读取周期计划失败', log: false });
		}
	}

	async function refresh() {
		loading = true;
		error = null;
		try {
			workflows = await listWorkflowDefinitions();
			if (selectedId && !workflows.some((workflow) => workflow.workflowId === selectedId)) {
				selectedId = null;
			}
		} catch (cause) {
			error = '无法读取已保存的 Workflow。';
			reportError(cause, { context: 'WorkflowLibrary', message: error, log: false, notify: false });
		} finally {
			loading = false;
		}
	}

	function clearDraft() {
		selectedId = null;
		name = '';
		description = '';
		definitionText = '{\n  "steps": []\n}';
		error = null;
	}

	function selectWorkflow(workflow: WorkflowDefinitionDto) {
		selectedId = workflow.workflowId;
		name = workflow.name;
		description = workflow.description;
		definitionText = JSON.stringify(workflow.definition, null, 2);
	}

	function copyWorkflow(workflow: WorkflowDefinitionDto) {
		selectedId = null;
		name = `${workflow.name} 副本`;
		description = workflow.description;
		definitionText = JSON.stringify(workflow.definition, null, 2);
	}

	async function save() {
		let definition: unknown;
		try {
			definition = JSON.parse(definitionText);
		} catch {
			error = 'Workflow JSON 格式无效。';
			return;
		}
		if (!definition || typeof definition !== 'object' || Array.isArray(definition)) {
			error = 'Workflow 定义必须是包含 steps 数组的 JSON 对象。';
			return;
		}
		saving = true;
		error = null;
		try {
			const saved = await saveWorkflowDefinition({
				workflowId: selectedId,
				expectedRevision: selected?.revision ?? null,
				name,
				description,
				definition,
			});
			await refresh();
			selectWorkflow(saved);
			addNotification(`Workflow「${saved.name}」已保存为第 ${saved.revision} 版`, 'success', 2500);
		} catch (cause) {
			reportError(cause, { context: 'WorkflowLibrary', message: '保存 Workflow 失败', log: false });
			await refresh();
			error = '保存失败。当前草稿已保留；请检查最新 revision 后再保存。';
		} finally {
			saving = false;
		}
	}

	async function removeWorkflow(workflow: WorkflowDefinitionDto) {
		if (!window.confirm(`删除 Workflow「${workflow.name}」？已创建的运行会保留其定义快照。`)) return;
		try {
			await deleteWorkflowDefinition(workflow.workflowId);
			if (selectedId === workflow.workflowId) clearDraft();
			await refresh();
			addNotification(`Workflow「${workflow.name}」已删除`, 'success', 2000);
		} catch (cause) {
			reportError(cause, { context: 'WorkflowLibrary', message: '删除 Workflow 失败', log: false });
		}
	}

	async function runSelectedWorkflow() {
		if (!selected || !sessionId) return;
		running = true;
		try {
			const toolRunId = await runWorkflowDefinition(selected.workflowId, sessionId);
			addNotification(`Workflow「${selected.name}」已排入任务 ${toolRunId}`, 'success', 3500);
		} catch (cause) {
			reportError(cause, { context: 'WorkflowLibrary', message: '启动 Workflow 失败', log: false });
		} finally {
			running = false;
		}
	}

	async function createSchedule() {
		if (!selected || !sessionId) return;
		const minutes = Number(intervalMinutes);
		if (!Number.isSafeInteger(minutes) || minutes < 1 || minutes > 525600) {
			error = '周期必须在 1 分钟到 1 年之间。';
			return;
		}
		const intervalSeconds = minutes * 60;
		scheduling = true;
		error = null;
		try {
			const anchorAt = new Date(Date.now() + intervalSeconds * 1000).toISOString();
			const schedule = await createWorkflowSchedule({
				workflowId: selected.workflowId,
				sessionId,
				intervalSeconds,
				anchorAt,
			});
			await refreshSchedules();
			addNotification(`已创建周期计划「${schedule.name}」`, 'success', 2500);
		} catch (cause) {
			reportError(cause, { context: 'WorkflowLibrary', message: '创建周期计划失败', log: false });
		} finally {
			scheduling = false;
		}
	}

	async function toggleSchedule(schedule: WorkflowScheduleDto) {
		scheduleBusyId = schedule.scheduleId;
		try {
			await setWorkflowScheduleEnabled(schedule.scheduleId, !schedule.enabled);
			await refreshSchedules();
		} catch (cause) {
			reportError(cause, { context: 'WorkflowLibrary', message: '更新周期计划失败', log: false });
		} finally {
			scheduleBusyId = null;
		}
	}

	async function removeSchedule(schedule: WorkflowScheduleDto) {
		if (!window.confirm(`删除周期计划「${schedule.name}」？当前运行会完成，但后续不再触发。`)) return;
		scheduleBusyId = schedule.scheduleId;
		try {
			await deleteWorkflowSchedule(schedule.scheduleId);
			await refreshSchedules();
			addNotification(`周期计划「${schedule.name}」已删除`, 'success', 2500);
		} catch (cause) {
			reportError(cause, { context: 'WorkflowLibrary', message: '删除周期计划失败', log: false });
		} finally {
			scheduleBusyId = null;
		}
	}

	function runtimeSessionLabel(session: RuntimeSessionListResponse['sessions'][number]): string {
		return session.title?.trim() || session.inputText.trim().slice(0, 64) || session.sessionId;
	}

	function formatNextDue(value: string): string {
		const date = new Date(value);
		return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
	}
</script>

<section class="workflow-library" aria-label="Workflow 定义库">
	<header class="library-heading">
		<div>
			<h2>Workflow 定义库</h2>
			<p>保存可复用的静态步骤图。运行时仍会按当前工具目录和授权策略逐步校验。</p>
		</div>
		<MaterialButton variant="outlined" label="新建 Workflow" onclick={clearDraft} />
	</header>

	{#if loading}
		<AsyncState state="loading" title="正在加载 Workflow" message="读取本地保存的定义。" />
	{:else if error && workflows.length === 0}
		<AsyncState state="error" title="Workflow 加载失败" message={error} actionLabel="重试" onAction={refresh} />
	{:else}
		<div class="library-layout">
			<div class="workflow-list" aria-label="已保存的 Workflow">
				{#if workflows.length === 0}
					<AsyncState title="暂无 Workflow" message="创建一个定义后，可在这里编辑、复制和复用。" />
				{:else}
					{#each workflows as workflow (workflow.workflowId)}
						<article class="workflow-row" class:selected={selectedId === workflow.workflowId}>
							<button class="workflow-select" type="button" onclick={() => selectWorkflow(workflow)}>
								<strong>{workflow.name}</strong>
								<span>第 {workflow.revision} 版 · {workflow.description || '无描述'}</span>
							</button>
							<div class="workflow-row-actions">
								<button type="button" onclick={() => copyWorkflow(workflow)}>复制</button>
								<button type="button" onclick={() => removeWorkflow(workflow)}>删除</button>
							</div>
						</article>
					{/each}
				{/if}
			</div>

			<div class="workflow-editor">
				<h3>{selected ? `编辑：${selected.name}` : '定义 Workflow'}</h3>
				<label>名称<input class="md-input" bind:value={name} maxlength="80" required /></label>
				<label>描述<input class="md-input" bind:value={description} maxlength="1000" /></label>
				<label class="definition-label">静态步骤图（JSON）
					<textarea class="md-input definition-editor" bind:value={definitionText} spellcheck="false" aria-label="Workflow JSON 定义"></textarea>
				</label>
				{#if error}<p class="editor-error" role="alert">{error}</p>{/if}
				<div class="editor-actions">
					<label class="session-picker">运行会话
						<select class="md-input" bind:value={sessionId} disabled={runtimeSessions.length === 0}>
							{#each runtimeSessions as session (session.sessionId)}
								<option value={session.sessionId}>{runtimeSessionLabel(session)}</option>
							{/each}
						</select>
					</label>
					<MaterialButton variant="outlined" label={running ? '排入中…' : '立即运行'} disabled={!selected || hasUnsavedChanges || !sessionId || running} onclick={() => void runSelectedWorkflow()} />
					<MaterialButton variant="filled" label={saving ? '保存中…' : '保存定义'} disabled={saving} onclick={() => void save()} />
				</div>
			</div>
			{#if selected && hasUnsavedChanges}<p class="schedule-empty">保存当前修改后，才能运行或创建该版本的周期计划。</p>{/if}
		</div>

		<section class="schedule-panel" aria-label="Workflow 周期计划">
			<div class="schedule-heading">
				<div><h3>周期计划</h3><p>每个到期周期创建独立 ToolRun；同一计划不会并行重叠。</p></div>
				<MaterialButton variant="outlined" label="刷新" onclick={() => { void refreshRuntimeSessions(); void refreshSchedules(); }} />
			</div>
			<div class="schedule-create">
				<label>运行会话
					<select class="md-input" bind:value={sessionId} disabled={runtimeSessions.length === 0}>
						{#each runtimeSessions as session (session.sessionId)}
							<option value={session.sessionId}>{runtimeSessionLabel(session)}</option>
						{/each}
					</select>
				</label>
				<label>间隔（分钟）<input class="md-input" type="number" min="1" max="525600" step="1" bind:value={intervalMinutes} /></label>
				<MaterialButton variant="filled" label={scheduling ? '创建中…' : '为当前定义创建计划'} disabled={!selected || hasUnsavedChanges || !sessionId || scheduling} onclick={() => void createSchedule()} />
			</div>
			{#if runtimeSessions.length === 0}<p class="schedule-empty">没有可用的运行中会话，请先打开或创建一个会话。</p>{/if}
			{#if schedules.length === 0}
				<p class="schedule-empty">暂无周期计划。</p>
			{:else}
				<div class="schedule-list">
					{#each schedules as schedule (schedule.scheduleId)}
						<article class="schedule-row">
							<div class="schedule-info">
								<strong>{schedule.name}</strong>
								<span>{Math.round(schedule.intervalSeconds / 60)} 分钟 · 下次 {formatNextDue(schedule.nextDueAt)}{schedule.activeToolRunId ? ' · 有运行进行中' : ''}</span>
							</div>
							<div class="schedule-actions">
								<button type="button" disabled={scheduleBusyId === schedule.scheduleId} onclick={() => void toggleSchedule(schedule)}>{schedule.enabled ? '暂停' : '启用'}</button>
								<button type="button" disabled={scheduleBusyId === schedule.scheduleId} onclick={() => void removeSchedule(schedule)}>删除</button>
							</div>
						</article>
					{/each}
				</div>
			{/if}
		</section>
	{/if}
</section>

<style>
	.workflow-library { display: flex; flex-direction: column; gap: var(--md-sys-space-lg); min-width: 0; }
	.library-heading { display: flex; align-items: flex-start; justify-content: space-between; gap: var(--md-sys-space-md); }
	h2, h3, p { margin: 0; }
	.library-heading p { color: var(--md-sys-color-on-surface-variant); margin-top: var(--md-sys-space-xs); }
	.library-layout { display: grid; grid-template-columns: minmax(240px, 0.85fr) minmax(320px, 1.5fr); gap: var(--md-sys-space-lg); align-items: start; }
	.workflow-list, .workflow-editor { min-width: 0; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-corner-lg); padding: var(--md-sys-space-md); }
	.workflow-list { display: flex; flex-direction: column; gap: var(--md-sys-space-sm); }
	.workflow-row { display: flex; justify-content: space-between; align-items: center; gap: var(--md-sys-space-sm); padding: var(--md-sys-space-sm); border-radius: var(--md-sys-shape-corner-md); background: var(--md-sys-color-surface-container-low); }
	.workflow-row.selected { outline: 2px solid var(--md-sys-color-primary); }
	.workflow-select { display: flex; flex: 1; min-width: 0; flex-direction: column; align-items: flex-start; gap: 4px; color: inherit; background: none; border: 0; text-align: left; cursor: pointer; }
	.workflow-select span { color: var(--md-sys-color-on-surface-variant); overflow-wrap: anywhere; }
	.workflow-row-actions { display: flex; gap: var(--md-sys-space-xs); }
	.workflow-row-actions button { background: none; border: 0; color: var(--md-sys-color-primary); cursor: pointer; }
	.workflow-editor { display: flex; flex-direction: column; gap: var(--md-sys-space-md); }
	.workflow-editor label { display: flex; flex-direction: column; gap: var(--md-sys-space-xs); }
	.definition-editor { min-height: 360px; width: 100%; box-sizing: border-box; resize: vertical; font-family: var(--md-sys-typescale-body-small-font); white-space: pre; }
	.editor-actions { display: flex; justify-content: flex-end; }
	.session-picker { min-width: min(100%, 260px); }
	.schedule-panel { display: flex; flex-direction: column; gap: var(--md-sys-space-md); min-width: 0; border: 1px solid var(--md-sys-color-outline-variant); border-radius: var(--md-sys-shape-corner-lg); padding: var(--md-sys-space-md); }
	.schedule-heading { display: flex; justify-content: space-between; align-items: flex-start; gap: var(--md-sys-space-md); }
	.schedule-heading p, .schedule-empty { color: var(--md-sys-color-on-surface-variant); margin: var(--md-sys-space-xs) 0 0; }
	.schedule-create { display: flex; align-items: end; gap: var(--md-sys-space-md); flex-wrap: wrap; }
	.schedule-create label { display: flex; flex-direction: column; gap: var(--md-sys-space-xs); }
	.schedule-create input { width: 160px; }
	.schedule-list { display: flex; flex-direction: column; gap: var(--md-sys-space-sm); }
	.schedule-row { display: flex; justify-content: space-between; align-items: center; gap: var(--md-sys-space-md); padding: var(--md-sys-space-sm); border-radius: var(--md-sys-shape-corner-md); background: var(--md-sys-color-surface-container-low); }
	.schedule-info { display: flex; flex-direction: column; gap: 4px; min-width: 0; }
	.schedule-info span { color: var(--md-sys-color-on-surface-variant); overflow-wrap: anywhere; }
	.schedule-actions { display: flex; gap: var(--md-sys-space-xs); }
	.schedule-actions button { background: none; border: 0; color: var(--md-sys-color-primary); cursor: pointer; }
	.editor-error { color: var(--md-sys-color-error); }
	@media (max-width: 760px) { .library-heading { flex-direction: column; } .library-layout { grid-template-columns: 1fr; } }
</style>
