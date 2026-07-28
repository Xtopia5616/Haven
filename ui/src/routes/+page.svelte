<script>
	import logger from '$lib/logger.js';
	import { onMount, onDestroy, tick } from 'svelte';
	import { browser } from '$app/environment';
	import { fly } from 'svelte/transition';
	import { get } from 'svelte/store';
	import { invoke, listen } from '$lib/tauri.js';
	import { taskMessagesStore, taskStore, addNotification, addTaskMessage, updateTaskMessages, adoptDraftMessages, clearTaskMessages, truncateTaskMessages, reviewTargetStore, activeTaskIdStore, seqLastSeen, pruneSeq, updateModelState } from '$lib/stores.js';
	import ChatBubble from '$lib/ChatBubble.svelte';
	import ConfirmationDialog from '$lib/ConfirmationDialog.svelte';
	import BranchDialog from '$lib/BranchDialog.svelte';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import Logo from '$lib/Logo.svelte';

	let transcriptInput = $state('');
	let messages = $state([]);
	let tasks = $state([]);
	let confirmDialog = $state({ stepId: null, toolName: '', taskId: '', riskLevel: 'medium' });
	let cancelConfirm = $state({ open: false, taskId: '', taskTitle: '' });
	let activeTaskId = $state(get(activeTaskIdStore));
	let branchDialog = $state({ open: false, stepNumber: null });
	let branchLoading = $state(false);

	// Right-click context menu state
	let ctxMenu = $state({ open: false, x: 0, y: 0, stepNumber: null, content: '', role: '', msgId: '' });

	function handleContextMenu(ev) {
		ctxMenu = { open: true, x: ev.x, y: ev.y, stepNumber: ev.stepNumber, content: ev.content, role: ev.role, msgId: ev.messageId };
	}

	// Rollback: find step number from click context or parse from message id
	function getStepForCtxMenu() {
		if (ctxMenu.stepNumber != null) return ctxMenu.stepNumber;
		// For user messages, look forward in the message list to the next
		// assistant message that carries a stepNumber.
		if (ctxMenu.role === 'user' && ctxMenu.msgId) {
			const idx = messages.findIndex(m => m.id === ctxMenu.msgId);
			if (idx >= 0) {
				const next = messages.slice(idx + 1).find(m => m.stepNumber != null);
				if (next) return next.stepNumber;
			}
		}
		return null;
	}

	function handleCtxRollback() {
		const step = getStepForCtxMenu();
		if (step == null) { addNotification('无法确定此消息对应的步骤', 'error', 3000); closeCtxMenu(); return; }
		branchDialog = { open: true, stepNumber: step };
		closeCtxMenu();
	}

	async function handleCtxCopy() {
		if (ctxMenu.content) {
			try { await navigator.clipboard.writeText(ctxMenu.content); addNotification('已复制', 'info', 1500); }
			catch { addNotification('复制失败', 'error', 2000); }
		}
		closeCtxMenu();
	}

	function closeCtxMenu() {
		ctxMenu = { open: false, x: 0, y: 0, stepNumber: null, content: '', role: '', msgId: '' };
	}

	function handleWindowClick(e) {
		if (!ctxMenu.open) return;
		const el = document.querySelector('.ctx-menu');
		if (el && !el.contains(e.target)) closeCtxMenu();
	}

	function handleWindowContextMenu(e) {
		if (ctxMenu.open) closeCtxMenu();
	}

	// Merged into existing onMount/onDestroy below

	async function confirmBranchAction() {
		const { stepNumber } = branchDialog;
		branchLoading = true;
		try {
			await invoke('rollback_task', { taskId: activeTaskId, targetStep: stepNumber });
			// Drop messages from the discarded timeline so the UI matches
			// the restored snapshot. The ReAct loop will re-stream steps
			// from `stepNumber` onward as it re-executes.
			truncateTaskMessages(activeTaskId, stepNumber);
			addNotification(`已回退到第 ${stepNumber} 步`, 'info', 3000);
		} catch (e) {
			addNotification(`回退失败: ${e}`, 'error', 5000);
		}
		branchLoading = false;
		branchDialog = { open: false, stepNumber: null };
		await loadTasks();
	}

	async function doCancelTask() {
		if (!cancelConfirm.taskId) return;
		try {
			await invoke('cancel_task', { taskId: cancelConfirm.taskId });
		} catch (e) {
			addNotification(`取消失败: ${e}`, 'error', 3000);
		}
		cancelConfirm = { open: false, taskId: '', taskTitle: '' };
	}

	function newTask() {
		if (activeTaskId) clearTaskMessages(activeTaskId);
		activeTaskId = null;
		activeTaskIdStore.set(null);
	}

	function endTask() {
		if (activeTaskId) {
			invoke('end_task', { taskId: activeTaskId }).catch((e) => {
				addNotification(`结束任务失败: ${e}`, 'error', 3000);
			});
			clearTaskMessages(activeTaskId);
		}
		activeTaskId = null;
		activeTaskIdStore.set(null);
	}

	let unlisteners = [];
	let messagesEl;
	let userScrolledUp = false;
	let dead = false;

	// Sync the Svelte store to a $state variable — $effect does NOT track
	// get(store), so we must use .subscribe() to get reactive updates.
	// Also read the current value once on mount via get(), otherwise values
	// set before subscription (e.g. by history review) are never received.
	let taskMessagesDict = $state({});
	$effect(() => {
		taskMessagesDict = get(taskMessagesStore);
		const unsub = taskMessagesStore.subscribe((v) => { taskMessagesDict = v; });
		return unsub;
	});

	// Derive visible messages for the current view.
	$effect(() => {
		const dict = taskMessagesDict;
		if (activeTaskId) {
			messages = Array.isArray(dict[activeTaskId]) ? dict[activeTaskId] : [];
		} else {
			messages = Array.isArray(dict['_draft']) ? dict['_draft'] : [];
		}
	});

	async function safeListen(event, handler) {
		try {
			const unsub = await listen(event, handler);
			unlisteners.push(unsub);
		} catch (e) {
			logger.error('+page', `Failed to register listener for '${event}'`, e);
		}
	}

	let activeTasks = $derived(
		tasks.filter((t) => t.status === 'running' || t.status === 'pending' || t.status === 'paused'),
	);

	// Auto-scroll to the newest message whenever messages change
	// or when any message is still streaming.
	$effect(() => {
		const _ = messages;
		const hasStreaming = messages.some((m) => m.streaming);
		if (hasStreaming || messages.length > 0) {
			scrollToBottom();
		}
	});

	// Persist activeTaskId across page navigations via store.
	$effect(() => {
		activeTaskIdStore.set(activeTaskId);
	});

	function scrollToBottom() {
		if (!messagesEl || userScrolledUp || dead) return;
		tick().then(() => {
			requestAnimationFrame(() => {
				if (dead) return;
				messagesEl.scrollTop = messagesEl.scrollHeight;
			});
		});
	}

	function onScroll() {
		if (!messagesEl) return;
		const threshold = 100;
		const atBottom =
			messagesEl.scrollHeight - messagesEl.scrollTop - messagesEl.clientHeight < threshold;
		userScrolledUp = !atBottom;
	}

	onMount(async () => {
		// Process review target first so loadTasks won't overwrite
		// activeTaskId with a stale paused task whose messages are gone.
		const reviewTarget = get(reviewTargetStore);
		if (reviewTarget && reviewTarget.taskId) {
			activeTaskId = reviewTarget.taskId;
			activeTaskIdStore.set(activeTaskId);
			// Defer clearing so it survives rapid remounts during init.
			setTimeout(() => reviewTargetStore.set(null), 0);
		}

		await loadTasks();

		if (!reviewTarget && activeTaskId && !tasks.some(t => t.id === activeTaskId)) {
			activeTaskId = null;
			activeTaskIdStore.set(null);
		}

		try {
			await safeListen('task:created', () => {
				loadTasks();
			});
			await safeListen('task:updated', () => {
				loadTasks();
			});
			await safeListen('task:completed', () => {
				loadTasks();
			});
			await safeListen('task:error', () => {
				loadTasks();
			});
			await safeListen('task:title-updated', (event) => {
				const { task_id, title } = event.payload;
				const idx = tasks.findIndex(t => t.id === task_id);
				if (idx >= 0) tasks[idx] = { ...tasks[idx], title };
			});
			await safeListen('agent:thought', (event) => {
				const data = event.payload;
				const tid = data.task_id;
				const stepId = `thought-${tid}-${data.step_number}-${data.run_id ?? 0}`;
				const reasoningId = `reasoning-${tid}-${data.step_number}-${data.run_id ?? 0}`;
				pruneSeq(stepId);
				pruneSeq(reasoningId);
				updateModelState('ready');
				updateTaskMessages(tid, (m) => {
					const reasoningFixed = m.map((x) =>
						x.id === reasoningId ? { ...x, streaming: false } : x
					);
					const idx = reasoningFixed.findIndex((x) => x.id === stepId);
					if (idx >= 0) {
						const next = [...reasoningFixed];
						next[idx] = { ...next[idx], content: data.thought, streaming: false, type: undefined };
						return next;
					}
					return [...reasoningFixed, {
						id: stepId, role: 'assistant', content: data.thought,
						type: undefined, voice: false, stepNumber: data.step_number,
						time: new Date().toLocaleTimeString(), streaming: false,
					}];
				});
			});
			function listenChunk(eventName, stepIdPrefix, msgType) {
				return safeListen(eventName, (event) => {
					const data = event.payload;
					const tid = data.task_id;
					const stepId = `${stepIdPrefix}-${tid}-${data.step_number}-${data.run_id ?? 0}`;
					const delta = data.delta || '';
					const seq = data.seq;
					updateModelState('streaming');
					if (seqLastSeen(stepId, seq)) return;
					if (!delta) return;
					updateTaskMessages(tid, (m) => {
						const idx = m.findIndex((x) => x.id === stepId);
						if (idx >= 0 && m[idx].streaming === false) return m;
						if (idx >= 0) {
							const curr = m[idx].content || '';
							// Some non-OpenAI providers send cumulative text per chunk
							const content = delta.startsWith(curr) ? delta : curr + delta;
							const next = [...m];
							next[idx] = { ...next[idx], content, streaming: true };
							return next;
						}
						return [...m, {
							id: stepId, role: 'assistant', content: delta,
							type: msgType, voice: false, stepNumber: data.step_number,
							time: new Date().toLocaleTimeString(), streaming: true,
						}];
					});
				});
			}
			await listenChunk('agent:thought_chunk', 'thought', undefined);
			await listenChunk('agent:reasoning_chunk', 'reasoning', 'reasoning');
			await safeListen('agent:supplement', () => {
			});
			await safeListen('agent:action', (event) => {
				const data = event.payload;
				if (data.silent) return;
				const tid = data.task_id;
				updateModelState('tool');
				const toolId = `tool-${tid}-${data.step_number}-${data.run_id ?? 0}-${data.tool_call_id || data.tool_name}`;
				const reasoningId = `reasoning-${tid}-${data.step_number}-${data.run_id ?? 0}`;
				pruneSeq(reasoningId);
				updateTaskMessages(tid, (m) => {
					const reasoningFixed = m.map((x) =>
						x.id === reasoningId ? { ...x, streaming: false } : x
					);
					const existing = reasoningFixed.find((x) => x.id === toolId);
					if (existing) return reasoningFixed;
					return [...reasoningFixed, {
						id: toolId,
						role: 'assistant',
						content: '',
						toolName: data.tool_name,
						type: 'tool',
						voice: false,
						stepNumber: data.step_number,
						time: new Date().toLocaleTimeString(),
						streaming: true,
					}];
				});
			});
			await safeListen('agent:observation', (event) => {
				const data = event.payload;
				if (data.silent) return;
				const tid = data.task_id;
				updateModelState('streaming');
				const toolId = `tool-${tid}-${data.step_number}-${data.run_id ?? 0}-${data.tool_call_id || data.tool_name}`;
				updateTaskMessages(tid, (m) => {
					const idx = m.findIndex((x) => x.id === toolId);
					if (idx >= 0) {
						const next = [...m];
						next[idx] = { ...next[idx], content: data.observation, streaming: false };
						return next;
					}
				return [...m, {
						id: toolId,
						role: 'assistant',
						content: data.observation,
						toolName: data.tool_name,
						type: 'tool',
						voice: false,
						stepNumber: data.step_number,
						streaming: false,
					}];
				});
			});
			await safeListen('confirm:requested', (event) => {
				const data = event.payload;
				if (data.task_id && activeTaskId && data.task_id !== activeTaskId) return;
				confirmDialog = {
					stepId: data.step_id,
					toolName: data.tool_name,
					taskId: data.task_id,
					riskLevel: data.risk_level || 'medium',
				};
			});
		} catch (e) {
			logger.warn('+page', 'safeListen error', e);
		}

		if (browser) {
			window.addEventListener('click', handleWindowClick);
			window.addEventListener('contextmenu', handleWindowContextMenu);
		}
	});

	onDestroy(() => {
		dead = true;
		unlisteners.forEach((u) => u());
		if (browser) {
			window.removeEventListener('click', handleWindowClick);
			window.removeEventListener('contextmenu', handleWindowContextMenu);
		}
	});

	async function loadTasks() {
		try {
			const result = await invoke('get_tasks');
			if (result && result.tasks) {
				tasks = result.tasks;
				taskStore.set(tasks);
				if (!activeTaskId) {
					const firstActive = tasks.find(
						(t) => t.status === 'running' || t.status === 'pending' || t.status === 'paused'
					);
					if (firstActive) {
						activeTaskId = firstActive.id;
					}
				}
			}
		} catch (e) {
			logger.warn('+page', 'loadTasks error', e);
			addNotification('加载任务列表失败', 'error', 3000);
		}
	}

	async function handleSubmit() {
		const text = transcriptInput.trim();
		if (!text) return;
		transcriptInput = '';

		const taskId = activeTaskId || '_draft';
		addTaskMessage(taskId, {
			id: `${Date.now()}-u-${Math.random().toString(36).slice(2, 6)}`,
			role: 'user',
			content: text,
			voice: false,
			time: new Date().toLocaleTimeString(),
		});

		try {
			if (activeTaskId) {
				await invoke('supplement_task', { taskId: activeTaskId, text });
				loadTasks();
			} else {
				const result = await invoke('process_transcript', {
					transcript: text,
					activeTaskId: null,
				});
				if (result && result.TaskCreated) {
					adoptDraftMessages(result.TaskCreated);
					activeTaskId = result.TaskCreated;
					activeTaskIdStore.set(activeTaskId);
				}
				loadTasks();
			}
		} catch (e) {
			addNotification(`Error: ${e}`, 'error');
		}
	}

	function handleKeydown(e) {
		if (e.key === 'Enter' && !e.shiftKey) {
			e.preventDefault();
			handleSubmit();
		}
	}

	async function handleConfirm({ stepId, approved, trustSession }) {
		try {
			await invoke('resolve_confirmation', {
				stepId,
				confirmed: approved,
				trustSession: trustSession || false,
			});
		} catch (e) {
			addNotification(`确认失败: ${e}`, 'error', 3000);
		}
		confirmDialog = { stepId: null, toolName: '', taskId: '', riskLevel: 'medium' };
	}
</script>

<div class="chat-page">
	<ConfirmationDialog
		stepId={confirmDialog.stepId}
		toolName={confirmDialog.toolName}
		taskId={confirmDialog.taskId}
		riskLevel={confirmDialog.riskLevel}
		onConfirm={handleConfirm}
	/>

	<BranchDialog
		open={branchDialog.open}
		stepNumber={branchDialog.stepNumber}
		taskSummary={tasks.find(t => t.id === activeTaskId)?.summary || ''}
		loading={branchLoading}
		onConfirm={confirmBranchAction}
		onClose={() => { if (!branchLoading) branchDialog = { open: false, stepNumber: null }; }}
	/>

	<!-- Right-click context menu -->
	{#if ctxMenu.open}
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<div class="ctx-menu" style="left: {ctxMenu.x}px; top: {ctxMenu.y}px;">
			<button class="ctx-item" onclick={handleCtxRollback}>
				<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="1 4 1 10 7 10" /><path d="M3.51 15a9 9 0 1 0 2.13-9.36L1 10" /></svg>
				回退到此消息
			</button>
			<button class="ctx-item" onclick={handleCtxCopy}>
				<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2" /><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" /></svg>
				复制
			</button>
		</div>
	{/if}

	<MaterialDialog
		open={cancelConfirm.open}
		onClose={() => { cancelConfirm = { open: false, taskId: '', taskTitle: '' }; }}
		title="Cancel Task"
	>
		{#snippet children()}
			<p>{`确定要取消任务 "${cancelConfirm.taskTitle}" 吗？已执行的工具操作不可回滚。`}</p>
		{/snippet}
		{#snippet footer()}
			<button
				class="md-btn md-btn--text"
				onclick={() => { cancelConfirm = { open: false, taskId: '', taskTitle: '' }; }}>
				Cancel
			</button>
			<button class="md-btn md-btn--filled" style="background: var(--md-sys-color-error)" onclick={doCancelTask}>
				确定取消
			</button>
		{/snippet}
	</MaterialDialog>

	<div class="messages-area" bind:this={messagesEl} onscroll={onScroll}>
		{#if messages.length === 0}
			<div class="welcome" in:fly={{ y: 12, duration: 220 }}>
				<Logo size={48} />
				<h2>Haven</h2>
				<p>PC 语音助手 · 按 Ctrl+Shift+Space 开始录音，或直接输入指令</p>
			</div>
		{:else}
			<div class="message-list">
				{#each messages as msg (msg.id)}
					<ChatBubble
						role={msg.role}
						content={msg.content}
						type={msg.type}
						voice={msg.voice}
						time={msg.time}
						streaming={msg.streaming}
						toolName={msg.toolName ?? ''}
						messageId={msg.id}
						stepNumber={msg.stepNumber}
						onContextMenu={handleContextMenu}
					/>
				{/each}
			</div>
		{/if}
	</div>

	<div class="input-area">
		{#if activeTasks.length > 0}
			<div class="task-bar">
				<div class="task-bar-list">
					{#each activeTasks as task (task.id)}
						<div class="task-pill">
							<span class="task-pill-dot" class:running={task.status === 'running'}></span>
							<span class="task-pill-label">{task.title || task.summary || task.input || 'Task'}</span>
							<button
								class="task-pill-action"
								onclick={() => { newTask(); }}
								title="新任务"
								aria-label="新任务"
								type="button"
							>
								<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
									<line x1="12" y1="5" x2="12" y2="19" />
									<line x1="5" y1="12" x2="19" y2="12" />
								</svg>
							</button>
							<button
								class="task-pill-action stop"
								onclick={() => { endTask(); }}
								title="结束任务"
								aria-label="结束任务"
								type="button"
							>
								<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
									<rect x="6" y="6" width="12" height="12" rx="1" />
								</svg>
							</button>
						</div>
					{/each}
				</div>
			</div>
		{/if}

		<div class="input-row">
			<button
				class="md-btn md-btn--outlined"
				onclick={() => { newTask(); }}
				type="button"
			>
				新任务
			</button>
			{#if activeTaskId}
				<button
					class="md-btn md-btn--outlined end-task-btn"
					onclick={() => { endTask(); }}
					type="button"
				>
					结束任务
				</button>
			{/if}
			<input
				type="text"
				placeholder={activeTaskId ? '追加指令' : '输入指令，或按 Ctrl+Shift+Space 录音'}
				bind:value={transcriptInput}
				onkeydown={handleKeydown}
				class="md-input chat-input"
			/>
			<button
				class="md-icon-button md-icon-button--filled send-btn"
				onclick={handleSubmit}
				disabled={!transcriptInput.trim()}
				aria-label="发送"
				type="button"
			>
				<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
					<line x1="12" y1="19" x2="12" y2="5" />
					<polyline points="5 12 12 5 19 12" />
				</svg>
			</button>
		</div>
	</div>
</div>

<style>
	.chat-page {
		display: flex;
		flex-direction: column;
		flex: 1;
		min-height: 0;
	}
	.messages-area {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		padding: var(--md-sys-space-xs) var(--md-sys-space-xs) var(--md-sys-space-lg);
		max-width: 760px;
		margin: 0 auto;
		width: 100%;
	}
	.welcome {
		text-align: center;
		padding: var(--md-sys-space-4xl) 0 var(--md-sys-space-3xl);
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: var(--md-sys-space-md);
	}
	.welcome h2 {
		font-family: var(--md-ref-typeface-brand);
		font-size: 32px;
		font-weight: 700;
		letter-spacing: 0.5px;
		color: var(--md-sys-color-primary);
	}
	.welcome p {
		color: var(--md-sys-color-on-surface-variant);
		font-size: var(--md-sys-typescale-body-size, 14px);
		max-width: 420px;
	}
	.message-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-sm);
	}

	.input-area {
		border-top: 1px solid var(--md-sys-color-outline-variant);
		padding: var(--md-sys-space-sm) 0 var(--md-sys-space-lg);
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-sm);
		flex-shrink: 0;
		max-width: 760px;
		margin: 0 auto;
		width: 100%;
	}

	.task-bar {
		display: flex;
	}
	.task-bar-list {
		display: flex;
		flex-wrap: wrap;
		gap: var(--md-sys-space-xs);
	}
	.task-pill {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		height: var(--md-comp-button-touch-height);
		padding: 0 var(--md-sys-space-xs) 0 var(--md-sys-space-md);
		background: var(--md-sys-color-surface-container-high);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-full);
		color: var(--md-sys-color-on-surface);
		transition: border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			background-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.task-pill:hover {
		border-color: var(--md-sys-color-outline);
	}
	.task-pill-dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--md-sys-color-outline);
		flex-shrink: 0;
	}
	.task-pill-dot.running {
		background: var(--md-sys-color-success);
		box-shadow: 0 0 6px color-mix(in srgb, var(--md-sys-color-success) 60%, transparent);
		animation: task-pill-pulse 1.5s var(--md-sys-motion-easing-emphasized) infinite;
	}
	@keyframes task-pill-pulse {
		0%, 100% { opacity: 1; }
		50% { opacity: 0.4; }
	}
	.task-pill-label {
		font-size: 13px;
		font-weight: 500;
		max-width: 240px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.task-pill-action {
		position: relative;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: var(--md-comp-icon-button-size);
		height: var(--md-comp-icon-button-size);
		border: none;
		border-radius: var(--md-sys-shape-full);
		background: transparent;
		color: var(--md-sys-color-on-surface-variant);
		cursor: pointer;
		overflow: hidden;
		transition: background-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}
	.task-pill-action::after {
		content: '';
		position: absolute;
		inset: 0;
		background: currentColor;
		opacity: 0;
		pointer-events: none;
		transition: opacity var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.task-pill-action:hover::after {
		opacity: var(--md-sys-state-hover-opacity);
	}
	.task-pill-action:focus-visible::after {
		opacity: var(--md-sys-state-focus-opacity);
	}
	.task-pill-action:active::after {
		opacity: var(--md-sys-state-pressed-opacity);
	}
	.task-pill-action.stop:hover {
		color: var(--md-sys-color-error);
	}

	.input-row {
		display: flex;
		gap: var(--md-sys-space-sm);
		align-items: center;
	}
	.input-row :global(.md-btn) {
		height: var(--md-comp-button-touch-height);
	}
	.end-task-btn {
		--md-sys-color-primary: var(--md-sys-color-error);
		--md-sys-color-on-primary: var(--md-sys-color-on-error);
	}
	.end-task-btn:hover {
		color: var(--md-sys-color-on-error);
		background: var(--md-sys-color-error);
	}
	.chat-input {
		border-radius: var(--md-sys-shape-medium);
		height: var(--md-comp-button-touch-height);
		flex: 1;
		min-width: 0;
	}
	.chat-input:focus {
		border-radius: var(--md-sys-shape-medium);
	}
	.send-btn {
		flex-shrink: 0;
	}
	.ctx-menu {
		position: fixed; z-index: 1000;
		background: var(--md-sys-color-surface-container-high);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		padding: var(--md-sys-space-xs);
		box-shadow: var(--md-sys-elevation-2);
		min-width: 160px;
	}
	.ctx-item {
		display: flex; align-items: center; gap: var(--md-sys-space-sm);
		width: 100%; padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border: none; background: transparent; color: var(--md-sys-color-on-surface);
		font-size: 13px; font-family: inherit; cursor: pointer;
		border-radius: var(--md-sys-shape-small);
		transition: background var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}
	.ctx-item:hover {
		background: var(--md-sys-color-surface-container-highest);
	}
	.ctx-item svg {
		flex-shrink: 0;
	}
</style>
