<script>
	import logger from '$lib/logger.js';
	import { onMount, onDestroy, tick } from 'svelte';
	import { browser } from '$app/environment';
	import { fly } from 'svelte/transition';
	import { get } from 'svelte/store';
	import { invoke, listen } from '$lib/tauri.js';
	import { taskMessagesStore, taskStore, addNotification, addTaskMessage, updateTaskMessages, adoptDraftMessages, clearTaskMessages, clearSeqMap, truncateTaskMessages, branchTaskMessages, reviewTargetStore, activeTaskIdStore, seqLastSeen, pruneSeq, updateModelState } from '$lib/stores.js';
	import ChatBubble from '$lib/ChatBubble.svelte';
	import ConfirmationDialog from '$lib/ConfirmationDialog.svelte';
	import BranchDialog from '$lib/BranchDialog.svelte';
	import Logo from '$lib/Logo.svelte';

	let transcriptInput = $state('');
	let messages = $state([]);
	let tasks = $state([]);
	let confirmDialog = $state({ stepId: null, toolName: '', taskId: '', riskLevel: 'medium' });
	let activeTaskId = $state(get(activeTaskIdStore));
	let branchDialog = $state({ open: false, stepNumber: null, role: '', content: '', msgId: '' });
	let branchLoading = $state(false);

	// Right-click context menu state
	let ctxMenu = $state({ open: false, x: 0, y: 0, stepNumber: null, content: '', role: '', msgId: '', selectedContent: '' });

	function handleContextMenu(ev) {
		ctxMenu = { open: true, x: ev.x, y: ev.y, stepNumber: ev.stepNumber, content: ev.content, role: ev.role, msgId: ev.messageId, selectedContent: ev.selectedContent || '' };
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
		branchDialog = { open: true, stepNumber: step, role: ctxMenu.role, content: ctxMenu.content, msgId: ctxMenu.msgId };
		closeCtxMenu();
	}

	async function handleCtxBranch() {
		const step = getStepForCtxMenu();
		if (step == null) { addNotification('无法确定此消息对应的步骤', 'error', 3000); closeCtxMenu(); return; }
		if (!activeTaskId) { addNotification('没有活跃任务，无法创建分支', 'error', 3000); closeCtxMenu(); return; }
		const sourceTaskId = activeTaskId;
		const targetStep = step;
		closeCtxMenu();
		try {
			const newTaskId = await invoke('branch_task', { taskId: sourceTaskId, targetStep });
			branchTaskMessages(sourceTaskId, newTaskId, targetStep);
			activeTaskId = newTaskId;
			activeTaskIdStore.set(newTaskId);
			addNotification('已创建分支', 'info', 3000);
			await loadTasks();
		} catch (e) {
			addNotification(`创建分支失败: ${e}`, 'error', 5000);
		}
	}

	async function handleCtxCopy() {
		const text = ctxMenu.selectedContent || ctxMenu.content;
		if (text) {
			try { await navigator.clipboard.writeText(text); addNotification('已复制', 'info', 1500); }
			catch { addNotification('复制失败', 'error', 2000); }
		}
		closeCtxMenu();
	}

	function closeCtxMenu() {
		ctxMenu = { open: false, x: 0, y: 0, stepNumber: null, content: '', role: '', msgId: '', selectedContent: '' };
	}

	$effect(() => {
		if (!ctxMenu.open) return;
		tick().then(() => {
			const el = document.querySelector('.ctx-menu');
			if (!el) return;
			const rect = el.getBoundingClientRect();
			const vw = window.innerWidth;
			const vh = window.innerHeight;
			let { x, y } = ctxMenu;
			if (x + rect.width > vw - 8) x = Math.max(8, x - rect.width);
			if (y + rect.height > vh - 8) y = Math.max(8, y - rect.height);
			if (x !== ctxMenu.x || y !== ctxMenu.y) {
				ctxMenu = { ...ctxMenu, x, y };
			}
		});
	});

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
		const { stepNumber, role, content, msgId } = branchDialog;
		branchLoading = true;
		try {
			if (role === 'user') {
				// User-message rollback: pause the task and put the message
				// text back in the input box so the user can edit and re-send.
				await invoke('rollback_task', { taskId: activeTaskId, targetStep: stepNumber, pause: true });
				// Remove the user message and everything after it, keeping
				// messages before it. This avoids truncateTaskMessages, which
				// would match the user message itself if it has an inferred
				// stepNumber (review view).
				updateTaskMessages(activeTaskId, (m) => {
					const idx = m.findIndex((x) => x.id === msgId);
					if (idx === -1) return m;
					return m.slice(0, idx);
				});
				clearSeqMap(activeTaskId);
				transcriptInput = content;
				addNotification('已回退，请编辑后重新发送', 'info', 3000);
			} else {
				await invoke('rollback_task', { taskId: activeTaskId, targetStep: stepNumber, pause: false });
				truncateTaskMessages(activeTaskId, stepNumber);
				addNotification(`已回退到第 ${stepNumber} 步`, 'info', 3000);
			}
		} catch (e) {
			addNotification(`回退失败: ${e}`, 'error', 5000);
		}
		branchLoading = false;
		branchDialog = { open: false, stepNumber: null, role: '', content: '', msgId: '' };
		await loadTasks();
	}

	function newTask() {
		if (activeTaskId) clearTaskMessages(activeTaskId);
		suppressAutoTask = true;
		activeTaskId = null;
		activeTaskIdStore.set(null);
		// Allow loadTasks auto-assign after the current call stack unwinds.
		setTimeout(() => { suppressAutoTask = false; }, 0);
	}

	async function endTask() {
		if (!activeTaskId) return;
		suppressAutoTask = true;
		const endedId = activeTaskId;
		try {
			await invoke('end_task', { taskId: endedId });
			clearTaskMessages(endedId);
		} catch (e) {
			addNotification(`结束任务失败: ${e}`, 'error', 3000);
		}
		activeTaskId = null;
		activeTaskIdStore.set(null);
		suppressAutoTask = false;
	}

	async function handleContinue() {
		if (!activeTaskId) return;
		const tid = activeTaskId;
		taskErrorId = null;
		activeTaskError = false;
		try {
			await invoke('continue_task', { taskId: tid });
			// Remove only the last partial assistant message(s) — the
			// interrupted step's incomplete output. Previous completed
			// steps are kept. The backend already deleted these from DB.
			updateTaskMessages(tid, (m) => {
				let i = m.length;
				while (i > 0 && m[i - 1].role === 'assistant') i--;
				return i < m.length ? m.slice(0, i) : m;
			});
			clearSeqMap(tid);
			addNotification('正在继续生成…', 'info', 2000);
			await loadTasks();
		} catch (e) {
			addNotification(`继续失败: ${e}`, 'error', 5000);
		}
	}

	let unlisteners = [];
	let messagesEl;
	let autoFollow = true;
	let scrollRafPending = false;
	let dead = false;
	// Suppresses loadTasks() auto-assigning activeTaskId during explicit
	// end/new operations so a late task event doesn't resurrect an ended task.
	let suppressAutoTask = false;
	// Guards concurrent loadTasks() calls so a stale response can't overwrite
	// a newer one.
	let loadTasksSeq = 0;

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

	let activeTaskError = $state(false);
	let taskErrorId = $state(null);

	// Clear error state when the active task changes.
	$effect(() => {
		const _ = activeTaskId;
		if (taskErrorId && activeTaskId !== taskErrorId) {
			taskErrorId = null;
			activeTaskError = false;
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

	// Auto-scroll to the newest message whenever messages change.
	$effect(() => {
		const _ = messages;
		if (messages.length > 0) {
			scrollToBottom();
		}
	});

	// When the active task changes (e.g. switching to a reviewed task or
	// creating a new task), re-enable follow and scroll to the bottom.
	$effect(() => {
		const _ = activeTaskId;
		autoFollow = true;
		scrollToBottom();
	});

	// Persist activeTaskId across page navigations via store.
	$effect(() => {
		activeTaskIdStore.set(activeTaskId);
	});

	function scrollToBottom() {
		if (!messagesEl || dead || scrollRafPending) return;
		scrollRafPending = true;
		requestAnimationFrame(() => {
			scrollRafPending = false;
			// Re-check autoFollow here so a user scroll-up between the call
			// and the rAF callback is respected (not overridden).
			if (dead || !messagesEl || !autoFollow) return;
			messagesEl.scrollTop = messagesEl.scrollHeight;
		});
	}

	function onScroll() {
		if (!messagesEl) return;
		const threshold = 100;
		const atBottom =
			messagesEl.scrollHeight - messagesEl.scrollTop - messagesEl.clientHeight < threshold;
		autoFollow = atBottom;
	}

	onMount(async () => {
		// Process review target first so loadTasks won't overwrite
		// activeTaskId with a stale paused task whose messages are gone.
		const reviewTarget = get(reviewTargetStore);
		if (reviewTarget && reviewTarget.taskId) {
			activeTaskId = reviewTarget.taskId;
			activeTaskIdStore.set(activeTaskId);
			// If this task was errored when reviewed, show the continue button.
			// reopen_task already set it to Paused, but we still want the user
			// to see the option to retry the failed step.
			if (reviewTarget.wasError) {
				taskErrorId = reviewTarget.taskId;
				activeTaskError = true;
			}
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
		await safeListen('task:error', (event) => {
			const { task_id } = event.payload;
				if (task_id && task_id === activeTaskId) {
					taskErrorId = task_id;
					activeTaskError = true;
				}
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

			// When the first text chunk arrives, the reasoning phase is
				// over — finalize any streaming reasoning block for this step.
				// This runs BEFORE the empty-delta check so that even an empty
				// transition chunk finalizes reasoning.
				if (stepIdPrefix === 'thought') {
					const reasoningId = `reasoning-${tid}-${data.step_number}-${data.run_id ?? 0}`;
					let reasoningFinalized = false;
					updateTaskMessages(tid, (m) => {
						const rIdx = m.findIndex((x) => x.id === reasoningId && x.streaming);
						if (rIdx < 0) return m;
						reasoningFinalized = true;
						return m.map((x) =>
							x.id === reasoningId ? { ...x, streaming: false } : x
						);
					});
					if (reasoningFinalized) pruneSeq(reasoningId);
				}

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
				const thoughtId = `thought-${tid}-${data.step_number}-${data.run_id ?? 0}`;
				pruneSeq(reasoningId);
				pruneSeq(thoughtId);
				updateTaskMessages(tid, (m) => {
					// Finalize any streaming reasoning and thought blocks —
					// a tool action means the text/reasoning phase is over.
					const fixed = m.map((x) =>
						(x.id === reasoningId || x.id === thoughtId)
							? { ...x, streaming: false }
							: x
					);
					const existing = fixed.find((x) => x.id === toolId);
					if (existing) return fixed;
					return [...fixed, {
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
			// If a confirmation is already pending, auto-reject the previous
			// one so the backend doesn't wait forever for a resolve_confirmation
			// that the user will never see.
			if (confirmDialog.stepId) {
				invoke('resolve_confirmation', { stepId: confirmDialog.stepId, confirmed: false, trustSession: false }).catch(() => {});
			}
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
		const seq = ++loadTasksSeq;
		try {
			const result = await invoke('get_tasks');
			// Stale response guard: a newer loadTasks call superseded this one.
			if (seq !== loadTasksSeq) return;
			if (result && result.tasks) {
				tasks = result.tasks;
				taskStore.set(tasks);
				if (!activeTaskId && !suppressAutoTask) {
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
		autoFollow = true;

		const taskId = activeTaskId || '_draft';
		addTaskMessage(taskId, {
			id: `${Date.now()}-u-${Math.random().toString(36).slice(2, 6)}`,
			role: 'user',
			content: text,
			voice: false,
			time: new Date().toLocaleTimeString(),
		});

		try {
			const result = await invoke('process_transcript', {
				transcript: text,
				activeTaskId: activeTaskId || null,
			});
			if (result && result.TaskCreated) {
				adoptDraftMessages(result.TaskCreated);
				activeTaskId = result.TaskCreated;
				activeTaskIdStore.set(activeTaskId);
			}
			loadTasks();
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
		isUserMessage={branchDialog.role === 'user'}
		loading={branchLoading}
		onConfirm={confirmBranchAction}
		onClose={() => { if (!branchLoading) branchDialog = { open: false, stepNumber: null, role: '', content: '', msgId: '' }; }}
	/>

	<!-- Right-click context menu -->
	{#if ctxMenu.open}
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<div class="ctx-menu" style="left: {ctxMenu.x}px; top: {ctxMenu.y}px;">
			<button class="ctx-item" onclick={handleCtxRollback}>
				<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="1 4 1 10 7 10" /><path d="M3.51 15a9 9 0 1 0 2.13-9.36L1 10" /></svg>
				回退到此消息
			</button>
			<button class="ctx-item" onclick={handleCtxBranch}>
				<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="6" cy="6" r="3" /><circle cx="6" cy="18" r="3" /><path d="M6 9v6" /><path d="M18 9h-6a4 4 0 0 0-4 4v4" /><circle cx="18" cy="6" r="3" /></svg>
				创建分支
			</button>
			<button class="ctx-item" onclick={handleCtxCopy}>
				<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2" /><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" /></svg>
				复制
			</button>
		</div>
	{/if}

	<div class="messages-area" bind:this={messagesEl} onscroll={onScroll}>
		{#if messages.length === 0}
			<div class="welcome" in:fly={{ y: 12, duration: 220 }}>
				<Logo size={48} />
				<h2>Haven</h2>
				<p>PC 语音助手 · 按 Ctrl+Shift+Space 开始录音，或直接输入指令</p>
			</div>
		{:else}
			<div class="message-list">
				{#each messages as msg, i (msg.id)}
					{@const isLast = i === messages.length - 1}
					<ChatBubble
						role={msg.role}
						content={msg.content}
						type={msg.type}
						voice={msg.voice}
						time={msg.time}
						streaming={msg.streaming && isLast}
						toolName={msg.toolName ?? ''}
						messageId={msg.id}
						stepNumber={msg.stepNumber}
						onContextMenu={handleContextMenu}
					/>
				{/each}
			</div>
		{/if}
		{#if activeTaskError}
			<div class="continue-banner" in:fly={{ y: 8, duration: 200 }}>
				<span class="continue-text">生成中断</span>
				<button class="md-btn md-btn--filled continue-btn" onclick={handleContinue} type="button">
					<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polygon points="5 3 19 12 5 21 5 3" /></svg>
					继续生成
				</button>
			</div>
		{/if}
	</div>

	<div class="input-area">
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

	.continue-banner {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: var(--md-sys-space-md);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		max-width: 760px;
		margin: 0 auto;
		width: 100%;
	}
	.continue-text {
		color: var(--md-sys-color-error);
		font-size: 13px;
	}
	.continue-btn {
		gap: var(--md-sys-space-xs);
		font-size: 13px;
	}
</style>
