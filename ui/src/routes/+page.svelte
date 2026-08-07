<script module>
	// Per-session cache for the toolbar model switcher's model discovery.
	// Dev-mode page reloads (Vite HMR reconnect, window re-show, single-
	// instance re-entry) remount the chat view and would otherwise fire a
	// duplicate discover_models request against the same default endpoint.
	// Cache the result per base URL and share in-flight requests so reloads
	// reuse the list instead of re-hitting the provider's /models endpoint.
	const defaultModelsCache = {
		baseUrl: null,
		list: null,
		inflight: null,
	};
</script>

<script>
	import logger from '$lib/logger.js';
	import { buildReviewMessages, mergeLiveStreaming } from '$lib/reviewMessages.js';
	import {
		accumulateStreamChunk,
		applyThoughtSnap,
		stepId,
		toolId,
		finalizeStreamBlocks,
		newToolMessage,
	} from '$lib/streaming.js';
	import { onMount, onDestroy, tick } from 'svelte';
	import { browser } from '$app/environment';
	import { fly } from 'svelte/transition';
	import { get } from 'svelte/store';
	import { invoke } from '$lib/tauri.js';
	import { registerListeners } from '$lib/events.js';
	import {
		taskMessagesStore,
		taskStore,
		addNotification,
		updateTaskMessages,
		adoptDraftMessages,
		clearTaskMessages,
		clearSeqMap,
		truncateTaskMessages,
		branchTaskMessages,
		reviewTargetStore,
		activeTaskIdStore,
		taskTokenStatsStore,
		updateTaskTokenStats,
		clearTaskTokenStats,
		restoreTaskTokenStats,
		formatTokenCount,
		formatCostUsd,
		seqLastSeen,
		pruneSeq,
		updateModelState,
		modelStateStore,
		imageDataUrl,
		recordingOverlay,
		DRAFT_KEY,
	} from '$lib/stores.js';
	import { submitTranscript } from '$lib/submit.js';
	import { syncStore, syncStoreImmediate } from '$lib/syncStore.js';
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

	// Pending image attachments (multimodal): [{ mediaType, data }] with data
	// holding base64 bytes (no data: prefix). Filled by paste / file picker,
	// sent along with the next message, cleared on submit.
	let pendingImages = $state([]);

	// Pending non-image file attachments: [{ media_type, data, filename, size }].
	// Read as base64 when picked, persisted by the backend to disk and handed
	// to the agent as a path the file tool can read.
	let pendingFiles = $state([]);
	// Single hidden picker for both images and files; the picked items are
	// split by type on selection (images -> pendingImages, rest -> pendingFiles).
	let attachFileInput = $state(null);

	// Recording state (mirror of the global recordingOverlay store) so the
	// toolbar mic button can toggle start/stop inline.
	let recordingState = $state({ isRecording: false });
	$effect(() =>
		syncStore(recordingOverlay, (v) => {
			recordingState = v;
		}),
	);

	// Model switcher state: the registry catalog plus the current default
	// model name, displayed on the toolbar button and filtered in the menu.
	let modelMenuOpen = $state(false);
	let taskMenuOpen = $state(false);
	let modelOptions = $state([]);
	let currentModelName = $state('');
	let currentModelId = $state('');
	let currentEffort = $state('');
	// Provider built-in web search mode ("off" | "auto" | "always").
	// Defaults to off (opt-in); "auto" lets the model decide when to search.
	let currentWebSearch = $state('off');
	let transcriptTextarea = $state(null);
	// The configured recording hotkey binding, loaded from settings and kept
	// in sync via `hotkey:rebind` so placeholders show the real value.
	let hotkeyBinding = $state('Ctrl+Shift+Space');

	// Active task token stats (mirrored from taskTokenStatsStore so this page
	// can render a compact budget widget). Cleared when the active task
	// changes; updated on every `agent:usage` event.
	/**
	 * @typedef {object} TaskTokenStats
	 * @property {number} promptTokens
	 * @property {number} completionTokens
	 * @property {number} totalTokens
	 * @property {number} cumulativePromptTokens
	 * @property {number} cumulativeCompletionTokens
	 * @property {number} cumulativeTotalTokens
	 * @property {number|null} costUsd
	 * @property {number|null} cumulativeCostUsd
	 * @property {number|null} contextWindow
	 * @property {string|null} model
	 * @property {boolean} [estimated] - totals restored from a rough backend
	 *   estimate (task predates usage persistence), not real recorded usage.
	 */

	/** @type {TaskTokenStats | null} */
	let tokenStats = $state(null);
	$effect(() =>
		syncStore(taskTokenStatsStore, (m) => {
			tokenStats = activeTaskId
				? /** @type {TaskTokenStats | undefined} */ (m[activeTaskId]) || null
				: null;
		}),
	);
	// Clear per-task stats when the active task changes so a stale entry
	// from a previous task doesn't bleed into the new task's display.
	$effect(() => {
		const _ = activeTaskId;
		// Subscribe so any store change refreshes; the actual filter is in
		// the subscription above. This effect just guarantees an unsubscribed
		// task is wiped when the user starts a fresh conversation.
		if (!activeTaskId) tokenStats = null;
	});

	/**
	 * Context-window utilization for the active task. Returns
	 * `{ used, window, ratio }` where `used` is the last reported prompt
	 * token count (per-step, not cumulative) and `window` is the model's
	 * configured budget. Returns `null` when no data is available.
	 */
	const contextBudget = $derived.by(() => {
		if (!tokenStats) return null;
		const window = tokenStats.contextWindow || 0;
		const used = tokenStats.promptTokens || 0;
		if (!window) return null;
		const ratio = Math.min(1, used / window);
		return { used, window, ratio };
	});

	// Send/stop merged button: text takes priority (always send); with no
	// text and the agent actively generating output the button becomes
	// "stop task". Also mirrors the agent's model state so the button can
	// distinguish "generating right now" from an idle running task.
	let modelState = $state('ready');
	$effect(() =>
		syncStore(modelStateStore, (v) => {
			modelState = v;
		}),
	);
	const hasInput = $derived(
		transcriptInput.trim().length > 0 || pendingImages.length > 0 || pendingFiles.length > 0,
	);
	const isGenerating = $derived(modelState === 'streaming' || modelState === 'tool');
	const taskRunning = $derived(
		!!activeTaskId &&
			tasks.some(
				(t) => t.id === activeTaskId && (t.status === 'running' || t.status === 'pending'),
			),
	);
	// While the agent is generating, a sent message is delivered immediately
	// to the backend: the agent injects it in the gap between tool calls and
	// the final content, so it can steer the answer instead of waiting for
	// the whole turn to finish.
	// The merged send button becomes "stop task" only when there is no input
	// and the agent is actively working (generating output, a running/pending
	// task). With fresh input present, it always stays a send button.
	const stopMode = $derived(!hasInput && (isGenerating || taskRunning));
	// Tooltip for the idle token widget. While the active task is still
	// running (streaming, tool-calling, or queued) more `agent:usage`
	// events are expected, so "waiting" is accurate. A finished or
	// history-opened conversation with no persisted usage will never
	// receive events — show a neutral hint instead of waiting forever.
	const tokenStatsHint = $derived(isGenerating || taskRunning ? '等待 LLM 统计' : '暂无统计');
	// Tasks executing in parallel (running or waiting). When 2+ exist, the
	// new-task button turns into a switcher menu: switch to a parallel task
	// or start a new one. Otherwise the button keeps its default behavior.
	const parallelTasks = $derived(
		tasks.filter((t) => t.status === 'running' || t.status === 'pending'),
	);
	const showTaskMenu = $derived(parallelTasks.length >= 2);

	function buildTokenTooltip(/** @type {TaskTokenStats} */ s) {
		const parts = [];
		parts.push(`本轮 ${s.promptTokens || 0} → ${s.completionTokens || 0} tokens`);
		parts.push(`累计 ${s.cumulativeTotalTokens || 0} tokens`);
		if (s.model) parts.push(`模型 ${s.model}`);
		if (s.contextWindow) {
			const pct =
				s.promptTokens && s.contextWindow
					? `${((s.promptTokens / s.contextWindow) * 100).toFixed(0)}%`
					: '?';
			parts.push(`上下文 ${pct} / ${formatTokenCount(s.contextWindow)}`);
		}
		if (s.cumulativeCostUsd != null) parts.push(`费用 ${formatCostUsd(s.cumulativeCostUsd)}`);
		if (s.estimated) parts.push('估算值（历史对话，未计费）');
		return parts.join('\n');
	}

	const effortOptions = [
		{ value: '', label: '默认' },
		{ value: 'low', label: '低' },
		{ value: 'medium', label: '中' },
		{ value: 'high', label: '高' },
	];

	const webSearchOptions = [
		{ value: 'off', label: '关闭' },
		{ value: 'auto', label: '自动' },
		{ value: 'always', label: '总是' },
	];

	async function handleWebSearchSelect(value) {
		const label = webSearchOptions.find((o) => o.value === value)?.label || '关闭';
		try {
			await invoke('set_web_search', { role: 'default_model', mode: value });
			currentWebSearch = value;
			addNotification(`联网搜索: ${label}`, 'success', 2500);
		} catch (e) {
			addNotification(`设置联网搜索失败: ${e}`, 'error', 4000);
		}
	}

	async function handleRecordClick() {
		try {
			if (recordingState.isRecording) {
				// Optimistic stop: flip the overlay instantly; the backend
				// confirms via recording:stopped ~50 ms later.
				recordingOverlay.update((v) => ({ ...v, isRecording: false, visible: false }));
				try {
					await invoke('stop_recording');
				} catch (e) {
					recordingOverlay.update((v) => ({ ...v, isRecording: true, visible: true }));
					throw e;
				}
			} else {
				// Optimistic start: the button/overlay respond immediately so
				// the brief stream-startup wait (~90 ms) behind `start_recording`
				// is not perceived as a laggy click.
				recordingOverlay.update((v) => ({ ...v, isRecording: true, visible: true }));
				try {
					await invoke('start_recording');
				} catch (e) {
					recordingOverlay.update((v) => ({ ...v, isRecording: false, visible: false }));
					// The backend already emits `recording:error` with a
					// friendly message (surfaced as a notification by the
					// layout), so do not re-throw — that would show a second,
					// redundant error toast.
				}
			}
		} catch (e) {
			addNotification(`录音失败: ${e}`, 'error', 3000);
		}
	}

	async function handleModelSelect(m) {
		modelMenuOpen = false;
		try {
			await invoke('switch_model', { role: 'default_model', modelId: m.id });
			currentModelId = m.id;
			currentModelName = m.name || m.id;
			addNotification(`已切换默认模型: ${currentModelName}`, 'success', 3000);
		} catch (e) {
			addNotification(`切换模型失败: ${e}`, 'error', 4000);
		}
	}

	async function handleEffortSelect(value) {
		const label = effortOptions.find((o) => o.value === value)?.label || '默认';
		try {
			await invoke('set_reasoning_effort', { role: 'default_model', effort: value || null });
			currentEffort = value || '';
			addNotification(`思考强度: ${label}`, 'success', 2500);
		} catch (e) {
			addNotification(`设置思考强度失败: ${e}`, 'error', 4000);
		}
	}

	const MAX_IMAGE_BYTES = 10 * 1024 * 1024; // 10 MiB per image
	const MAX_IMAGES = 4;
	const MAX_FILE_BYTES = 20 * 1024 * 1024; // 20 MiB per file
	const MAX_FILES = 5;
	// Downscale images so the longest edge does not exceed this. OpenAI vision
	// guidance recommends ≤1568px; smaller payloads cut DB storage, snapshot
	// serialization, IPC transfer, and LLM token cost.
	const MAX_IMAGE_DIM = 1568;
	const JPEG_QUALITY = 0.85;

	/** Read a File as a { media_type, data } attachment without re-encoding. */
	function readAsAttachment(file) {
		return new Promise((resolve, reject) => {
			const reader = new FileReader();
			reader.onload = () => {
				const dataUrl = String(reader.result || '');
				const comma = dataUrl.indexOf(',');
				const base64 = comma >= 0 ? dataUrl.slice(comma + 1) : dataUrl;
				resolve({ media_type: file.type || 'application/octet-stream', data: base64 });
			};
			reader.onerror = () => reject(new Error('文件读取失败'));
			reader.readAsDataURL(file);
		});
	}

	/**
	 * Downscale and re-encode an image File to JPEG to reduce payload size.
	 * Returns null if compression isn't possible (e.g. browser lacks the API).
	 */
	async function tryCompressImage(file) {
		if (typeof createImageBitmap !== 'function' || typeof document === 'undefined') return null;
		try {
			const bitmap = await createImageBitmap(file);
			let { width, height } = bitmap;
			const maxDim = Math.max(width, height);
			if (maxDim > MAX_IMAGE_DIM) {
				const scale = MAX_IMAGE_DIM / maxDim;
				width = Math.round(width * scale);
				height = Math.round(height * scale);
			}
			const canvas = document.createElement('canvas');
			canvas.width = width;
			canvas.height = height;
			const ctx = canvas.getContext('2d');
			if (!ctx) return null;
			ctx.drawImage(bitmap, 0, 0, width, height);
			bitmap.close?.();
			const dataUrl = canvas.toDataURL('image/jpeg', JPEG_QUALITY);
			const comma = dataUrl.indexOf(',');
			return {
				media_type: 'image/jpeg',
				data: comma >= 0 ? dataUrl.slice(comma + 1) : dataUrl,
			};
		} catch (e) {
			logger.warn('+page', 'image compression failed, using original', e);
			return null;
		}
	}

	/**
	 * Convert a File to a { media_type, data } attachment (base64, no prefix).
	 * Compresses to JPEG when the result is smaller than the original;
	 * otherwise keeps the original encoding.
	 */
	async function fileToAttachment(file) {
		if (file.size > MAX_IMAGE_BYTES) {
			throw new Error('图片超过 10MB 上限');
		}
		const original = await readAsAttachment(file);
		const compressed = await tryCompressImage(file);
		if (compressed && compressed.data.length < original.data.length) {
			return compressed;
		}
		return original;
	}

	const IMAGE_EXTENSIONS = new Set([
		'png',
		'jpg',
		'jpeg',
		'gif',
		'webp',
		'bmp',
		'svg',
		'avif',
		'ico',
	]);

	/**
	 * Decide whether a picked file counts as an image (vision path) or a
	 * generic file (disk path) by MIME type first, then extension — so a
	 * `.png` with a missing/odd MIME still routes to the image logic.
	 */
	function isImageFile(file) {
		if (file.type && file.type.startsWith('image/')) return true;
		const ext = (file.name.split('.').pop() || '').toLowerCase();
		return IMAGE_EXTENSIONS.has(ext);
	}

	async function addPendingImages(files) {
		if (!files || files.length === 0) return;
		const room = MAX_IMAGES - pendingImages.length;
		if (room <= 0) {
			addNotification(`最多支持 ${MAX_IMAGES} 张图片`, 'error', 3000);
			return;
		}
		const list = Array.from(files).slice(0, room);
		for (const f of list) {
			if (!isImageFile(f)) {
				addNotification(`不支持的文件类型: ${f.name}`, 'error', 3000);
				continue;
			}
			try {
				pendingImages = [...pendingImages, await fileToAttachment(f)];
			} catch (e) {
				addNotification(e.message || '图片读取失败', 'error', 3000);
			}
		}
	}

	function handlePaste(e) {
		const items = e.clipboardData?.items;
		if (!items) return;
		const images = [];
		for (const item of items) {
			if (item.type.startsWith('image/')) {
				const file = item.getAsFile();
				if (file) images.push(file);
			}
		}
		if (images.length > 0) {
			e.preventDefault();
			addPendingImages(images);
		}
	}

	function removePendingImage(index) {
		pendingImages = pendingImages.filter((_, i) => i !== index);
	}

	function formatFileSize(bytes) {
		if (bytes < 1024) return `${bytes} B`;
		if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
		return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
	}

	// Read non-image files as base64 attachments (with the original name) so
	// the backend can persist them to disk and hand the agent a path. Files
	// are capped at MAX_FILES / MAX_FILE_BYTES, mirroring server validation.
	async function addPendingFiles(files) {
		if (!files || files.length === 0) return;
		const room = MAX_FILES - pendingFiles.length;
		if (room <= 0) {
			addNotification(`最多支持 ${MAX_FILES} 个文件`, 'error', 3000);
			return;
		}
		const list = Array.from(files).slice(0, room);
		for (const f of list) {
			if (f.size > MAX_FILE_BYTES) {
				addNotification(`文件超过 20MB 上限: ${f.name}`, 'error', 3000);
				continue;
			}
			try {
				const { media_type, data } = await readAsAttachment(f);
				pendingFiles = [
					...pendingFiles,
					{ media_type, data, filename: f.name, size: f.size },
				];
			} catch (e) {
				addNotification(e.message || '文件读取失败', 'error', 3000);
			}
		}
	}

	// Single entry point for the attachment picker: images (by MIME/extension)
	// go to the vision preview row, everything else to the file chips.
	function handleAttachSelect(e) {
		const files = Array.from(e.target.files || []);
		const images = files.filter(isImageFile);
		const others = files.filter((f) => !isImageFile(f));
		if (images.length > 0) addPendingImages(images);
		if (others.length > 0) addPendingFiles(others);
		e.target.value = '';
	}

	function removePendingFile(index) {
		pendingFiles = pendingFiles.filter((_, i) => i !== index);
	}

	// Right-click context menu state
	let ctxMenu = $state({
		open: false,
		x: 0,
		y: 0,
		stepNumber: null,
		content: '',
		role: '',
		msgId: '',
		selectedContent: '',
	});

	function handleContextMenu(ev) {
		ctxMenu = {
			open: true,
			x: ev.x,
			y: ev.y,
			stepNumber: ev.stepNumber,
			content: ev.content,
			role: ev.role,
			msgId: ev.messageId,
			selectedContent: ev.selectedContent || '',
		};
	}

	// Rollback: find step number from click context or parse from message id
	function getStepForCtxMenu() {
		if (ctxMenu.stepNumber != null) return ctxMenu.stepNumber;
		// For user messages, look forward in the message list to the next
		// assistant message that carries a stepNumber.
		if (ctxMenu.role === 'user' && ctxMenu.msgId) {
			const idx = messages.findIndex((m) => m.id === ctxMenu.msgId);
			if (idx >= 0) {
				const next = messages.slice(idx + 1).find((m) => m.stepNumber != null);
				if (next) return next.stepNumber;
			}
			// Fallback for an interrupted message that was never processed
			// (no step row and nothing after it — sent while the task was
			// erroring, or the app closed before the steering drained).
			// Target the step after the last completed one; the backend
			// discards just this message when no branch point covers it.
			const maxStep = messages.reduce(
				(acc, m) => (m.stepNumber != null ? Math.max(acc, m.stepNumber) : acc),
				0,
			);
			return maxStep + 1;
		}
		return null;
	}

	function handleCtxRollback() {
		const step = getStepForCtxMenu();
		if (step == null) {
			addNotification('无法确定此消息对应的步骤', 'error', 3000);
			closeCtxMenu();
			return;
		}
		branchDialog = {
			open: true,
			stepNumber: step,
			role: ctxMenu.role,
			content: ctxMenu.content,
			msgId: ctxMenu.msgId,
		};
		closeCtxMenu();
	}

	async function handleCtxBranch() {
		const step = getStepForCtxMenu();
		if (step == null) {
			addNotification('无法确定此消息对应的步骤', 'error', 3000);
			closeCtxMenu();
			return;
		}
		if (!activeTaskId) {
			addNotification('没有活跃任务，无法创建分支', 'error', 3000);
			closeCtxMenu();
			return;
		}
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
			try {
				await navigator.clipboard.writeText(text);
				addNotification('已复制', 'info', 1500);
			} catch {
				addNotification('复制失败', 'error', 2000);
			}
		}
		closeCtxMenu();
	}

	function closeCtxMenu() {
		ctxMenu = {
			open: false,
			x: 0,
			y: 0,
			stepNumber: null,
			content: '',
			role: '',
			msgId: '',
			selectedContent: '',
		};
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
		if (ctxMenu.open) {
			const el = document.querySelector('.ctx-menu');
			if (el && !el.contains(e.target)) closeCtxMenu();
		}
		if (modelMenuOpen) {
			const menu = document.querySelector('.model-menu');
			const btn = document.querySelector('.model-switch-btn');
			if (menu && btn && !menu.contains(e.target) && !btn.contains(e.target)) {
				modelMenuOpen = false;
			}
		}
		if (taskMenuOpen) {
			const menu = document.querySelector('.task-menu');
			const btn = document.querySelector('.task-switch-btn');
			if (menu && btn && !menu.contains(e.target) && !btn.contains(e.target)) {
				taskMenuOpen = false;
			}
		}
	}

	$effect(() => {
		if (taskMenuOpen && parallelTasks.length < 2) taskMenuOpen = false;
	});

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
				await invoke('rollback_task', {
					taskId: activeTaskId,
					targetStep: stepNumber,
					pause: true,
					targetMessageId: msgId,
				});
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
				await invoke('rollback_task', {
					taskId: activeTaskId,
					targetStep: stepNumber,
					pause: false,
					targetMessageId: msgId,
				});
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
		if (activeTaskId) {
			clearTaskMessages(activeTaskId);
			clearTaskTokenStats(activeTaskId);
		}
		suppressAutoTask = true;
		activeTaskId = null;
		activeTaskIdStore.set(null);
		// 新对话 = explicit fresh start: don't auto-restore the previous
		// conversation on the next app launch (cleared when a new task is
		// actually created).
		if (browser) localStorage.setItem('haven.no_auto_restore', '1');
		taskMenuOpen = false;
		if (parallelTasks.length === 0) {
			// Nothing running that could hijack the draft: allow loadTasks
			// auto-assign after the current call stack unwinds (e.g. a task
			// created by a voice transcript).
			setTimeout(() => {
				suppressAutoTask = false;
			}, 0);
		} else {
			// A task is still running in the background: stay on the fresh
			// draft until a new task is actually created, otherwise a task
			// event would auto-assign back to the running task and the next
			// message would be appended to it instead of starting a new one.
			suppressAutoTask = true;
		}
	}

	// Switch the chat view to another parallel task. Merges the persisted
	// DB messages with any in-memory streaming messages that arrived
	// concurrently (the task may still be running).
	async function switchToTask(taskId) {
		taskMenuOpen = false;
		try {
			const result = await invoke('get_task_for_review', { taskId });
			const dbMessages = buildReviewMessages(result);
			// Drop DB step badges for steps already represented by a live
			// tool card (the running task may be mid-step): the live card
			// keeps streaming its observation. Their ids differ, so plain
			// id dedup would leave both visible.
			updateTaskMessages(taskId, (existing) =>
				mergeLiveStreaming(dbMessages, existing, { dropToolSteps: true }),
			);
			restoreTaskTokenStats(taskId, result.usage, result.usage_estimated);
			suppressAutoTask = false;
			activeTaskId = taskId;
			activeTaskIdStore.set(taskId);
			const t = tasks.find((x) => x.id === taskId);
			addNotification(`已切换到：${t?.title || '任务'}`, 'info', 1500);
		} catch (e) {
			addNotification(`切换任务失败: ${e}`, 'error', 4000);
		}
	}

	async function endTask() {
		if (!activeTaskId) return;
		suppressAutoTask = true;
		const endedId = activeTaskId;
		try {
			await invoke('end_task', { taskId: endedId });
			clearTaskMessages(endedId);
			clearTaskTokenStats(endedId);
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
		// Capture the ids of the trailing assistant messages BEFORE invoking
		// continue_task. These are the partial outputs from the interrupted
		// step that the backend will delete from the DB. We must remove them
		// from the UI too, but only these — the dispatcher may start the retry
		// before this function resumes and append NEW assistant messages
		// (different run_id in their ids) that must NOT be dropped.
		const currentMessages = get(taskMessagesStore)[tid] || [];
		let trailingIdx = currentMessages.length;
		while (trailingIdx > 0 && currentMessages[trailingIdx - 1].role === 'assistant') {
			trailingIdx--;
		}
		const partialIds = new Set(currentMessages.slice(trailingIdx).map((m) => m.id));
		try {
			// First unblock the errored task: continue_task truncates the
			// partial output and sets the task to Pending so the "继续" user
			// message below is accepted instead of being dropped as a
			// terminal-state supplement.
			await invoke('continue_task', { taskId: tid });
			taskErrorId = null;
			activeTaskError = false;
			// Drop only the captured partial messages. New retry messages
			// (arrived during the await) have different ids and are kept.
			if (partialIds.size > 0) {
				updateTaskMessages(tid, (m) => {
					const filtered = m.filter((x) => !partialIds.has(x.id));
					return filtered.length !== m.length ? filtered : m;
				});
			}
			clearSeqMap(tid);
			// Continue by sending a real user message, so "继续" appears in
			// the conversation and is delivered to the agent as an
			// interjection, just like a typed or quick-reply message.
			autoFollow = true;
			submitMessage('继续', []);
			await loadTasks();
		} catch (e) {
			addNotification(`继续失败: ${e}`, 'error', 5000);
			// Keep the banner visible so the user can retry.
		}
	}

	// Tauri event listener handle (registered in onMount, disposed in
	// onDestroy). See eventRegistrations below.
	let eventRegistrations = null;
	let messagesEl;
	let autoFollow = $state(true);
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
	$effect(() =>
		syncStoreImmediate(
			taskMessagesStore,
			(v) => {
				taskMessagesDict = v;
			},
			get,
		),
	);

	// Derive visible messages for the current view.
	$effect(() => {
		const dict = taskMessagesDict;
		if (activeTaskId) {
			messages = Array.isArray(dict[activeTaskId]) ? dict[activeTaskId] : [];
		} else {
			messages = Array.isArray(dict[DRAFT_KEY]) ? dict[DRAFT_KEY] : [];
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

	// Cold-mount scroll for conversations opened as a bulk snapshot (history
	// review, app-start auto-restore): at that moment every bubble is
	// content-visibility-skipped and reports only its contain-intrinsic-size
	// estimate (~120px), so the first scrollToBottom lands above the real
	// bottom. Force one full render pass — the real sizes are then remembered
	// by `contain-intrinsic-size: auto` — scroll, and restore lazy rendering.
	function scrollToBottomAfterOpen() {
		if (dead || !messagesEl) return;
		const list = messagesEl.querySelector('.message-list');
		if (!list) return;
		const bubbles = list.querySelectorAll('.bubble');
		bubbles.forEach((b) => b.style.setProperty('content-visibility', 'visible'));
		messagesEl.scrollTop = messagesEl.scrollHeight;
		let frames = 2;
		const finish = () => {
			frames -= 1;
			if (frames > 0) {
				requestAnimationFrame(finish);
				return;
			}
			if (dead || !messagesEl) return;
			if (autoFollow) messagesEl.scrollTop = messagesEl.scrollHeight;
			bubbles.forEach((b) => b.style.removeProperty('content-visibility'));
		};
		requestAnimationFrame(finish);
	}

	function onScroll() {
		if (!messagesEl) return;
		const threshold = 100;
		const atBottom =
			messagesEl.scrollHeight - messagesEl.scrollTop - messagesEl.clientHeight < threshold;
		autoFollow = atBottom;
	}

	function jumpToBottom() {
		autoFollow = true;
		if (messagesEl) messagesEl.scrollTop = messagesEl.scrollHeight;
	}

	// Streaming chunk handler factory: finalizes the preceding reasoning block
	// on the first thought chunk, dedups by per-step seq, and accumulates the
	// delta into the in-memory message list.
	function chunkHandler(stepIdPrefix, msgType) {
		return (event) => {
			const data = event.payload;
			const tid = data.task_id;
			const sid = stepId(stepIdPrefix, tid, data.step_number, data.run_id);
			const delta = data.delta || '';
			const seq = data.seq;
			updateModelState('streaming');
			if (seqLastSeen(sid, seq)) return;

			// When the first text chunk arrives, the reasoning phase is
			// over — finalize any streaming reasoning block for this step.
			// This runs BEFORE the empty-delta check so that even an empty
			// transition chunk finalizes reasoning.
			if (stepIdPrefix === 'thought') {
				const reasoningId = stepId('reasoning', tid, data.step_number, data.run_id);
				let reasoningFinalized = false;
				updateTaskMessages(tid, (m) => {
					const rIdx = m.findIndex((x) => x.id === reasoningId && x.streaming);
					if (rIdx < 0) return m;
					reasoningFinalized = true;
					return m.map((x) => (x.id === reasoningId ? { ...x, streaming: false } : x));
				});
				if (reasoningFinalized) pruneSeq(reasoningId);
			}

			if (!delta) return;
			updateTaskMessages(tid, (m) =>
				accumulateStreamChunk(m, {
					stepId: sid,
					stepIdPrefix,
					delta,
					msgType,
					stepNumber: data.step_number,
					time: new Date().toLocaleTimeString(),
				}),
			);
		};
	}

	// Populate the toolbar model switcher from a per-session cache so page
	// reloads (dev HMR reconnect, window re-show, single-instance re-entry)
	// don't re-request the same model list. Concurrent mounts share the
	// in-flight request, so the duplicate discover_models calls seen on
	// reload disappear without losing the fresh-on-first-load behavior.
	function ensureDefaultModelOptions(baseUrl) {
		if (defaultModelsCache.baseUrl === baseUrl && defaultModelsCache.list) {
			modelOptions = defaultModelsCache.list;
			return;
		}
		if (defaultModelsCache.inflight) {
			defaultModelsCache.inflight
				.then((list) => {
					if (!dead) modelOptions = list;
				})
				.catch(() => {
					if (!dead) modelOptions = [];
				});
			return;
		}
		defaultModelsCache.baseUrl = baseUrl;
		defaultModelsCache.inflight = invoke('discover_models', {
			baseUrl,
			apiKey: '',
			role: 'default_model',
		})
			.then((list) => {
				const next = list || [];
				defaultModelsCache.list = next;
				if (!dead) modelOptions = next;
				return next;
			})
			.catch((e) => {
				logger.warn('+page', 'discover_models error', e);
				if (!dead) modelOptions = [];
				throw e;
			})
			.finally(() => {
				defaultModelsCache.inflight = null;
			});
		// Swallow the rethrown rejection for the shared in-flight promise;
		// the branch above already surfaces the failure to the UI.
		defaultModelsCache.inflight.catch(() => {});
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

		// Register listeners BEFORE any async data load so task/streaming
		// events arriving while the page initializes are never missed.
		const registrations = registerListeners(
			{
				'task:created': (event) => {
					const tid = event.payload?.task_id;
					if (tid) {
						// Voice input appends the transcript to `_draft` before the
						// backend task exists; once it is created, migrate those
						// draft messages into the task and focus it. Without this,
						// the agent's response (ask card / answer) lands in a task
						// stream the chat view is not showing — visible only after
						// re-entering the page (e.g. via history).
						adoptDraftMessages(tid);
						if (!suppressAutoTask) {
							activeTaskId = tid;
							activeTaskIdStore.set(tid);
						}
					}
					if (browser) localStorage.removeItem('haven.no_auto_restore');
					loadTasks();
				},
				'task:updated': (event) => {
					const data = event.payload || {};
					const isActive = data.task_id && activeTaskId && data.task_id === activeTaskId;
					// A resume (pending) means the user's answer was received:
					// stop showing the awaiting indicator on ask cards. Note the
					// ask pause itself arrives as 'paused' right after the card is
					// created, so that status must NOT clear the indicator.
					if (isActive && data.status === 'pending') {
						clearAskAwaiting(data.task_id);
					}
					loadTasks();
				},
				'task:completed': (event) => {
					const data = event.payload || {};
					if (data.task_id && activeTaskId && data.task_id === activeTaskId) {
						clearAskAwaiting(data.task_id);
					}
					loadTasks();
				},
				'task:error': (event) => {
					const { task_id } = event.payload;
					if (task_id && task_id === activeTaskId) {
						taskErrorId = task_id;
						activeTaskError = true;
						clearAskAwaiting(task_id);
					}
					loadTasks();
				},
				'task:title-updated': (event) => {
					const { task_id, title } = event.payload;
					const idx = tasks.findIndex((t) => t.id === task_id);
					if (idx >= 0) tasks[idx] = { ...tasks[idx], title };
				},
				'hotkey:rebind': (event) => {
					const data = event.payload || {};
					if (data.new_binding) {
						hotkeyBinding = data.new_binding;
					}
				},
				'agent:thought': (event) => {
					const data = event.payload;
					const tid = data.task_id;
					const thoughtId = stepId('thought', tid, data.step_number, data.run_id);
					const reasoningId = stepId('reasoning', tid, data.step_number, data.run_id);
					pruneSeq(thoughtId);
					pruneSeq(reasoningId);
					updateModelState('ready');
					updateTaskMessages(tid, (m) =>
						applyThoughtSnap(m, {
							stepId: thoughtId,
							reasoningId,
							thought: data.thought,
							stepNumber: data.step_number,
							time: new Date().toLocaleTimeString(),
						}),
					);
				},
				'agent:thought_chunk': chunkHandler('thought', undefined),
				'agent:reasoning_chunk': chunkHandler('reasoning', 'reasoning'),
				'agent:web_search': (event) => {
					const data = event.payload || {};
					const tid = data.task_id;
					if (!tid || (activeTaskId && tid !== activeTaskId)) return;
					const wsId = toolId(tid, data.step_number, data.run_id, 'web_search');
					updateTaskMessages(tid, (m) => {
						const existing = m.find((x) => x.id === wsId);
						if (data.phase === 'completed') {
							if (!existing) return m;
							return m.map((x) =>
								x.id === wsId
									? { ...x, streaming: false, content: '已联网搜索' }
									: x,
							);
						}
						// in_progress / searching: keep the indicator alive.
						const content = data.phase === 'searching' ? '正在搜索…' : '正在联网搜索…';
						if (existing) {
							return m.map((x) => (x.id === wsId ? { ...x, content } : x));
						}
						return [
							...m,
							newToolMessage({
								id: wsId,
								stepNumber: data.step_number,
								toolName: 'web_search',
								time: new Date().toLocaleTimeString(),
								content,
								streaming: true,
							}),
						];
					});
				},
				'agent:supplement': (event) => {
					// The agent injected a user message (mid-turn steering or a
					// resumed-task supplement) into its context. Mark the matching
					// user bubble as received so the user knows their input was
					// picked up mid-turn rather than deferred.
					const data = event.payload || {};
					const tid = data.task_id;
					const ctx = (data.additional_context || '').trim();
					if (!tid || !ctx) return;
					updateTaskMessages(tid, (m) => {
						let marked = false;
						const next = [...m];
						for (let i = next.length - 1; i >= 0; i--) {
							const x = next[i];
							if (
								x.role === 'user' &&
								!x.received &&
								(x.content || '').trim() === ctx
							) {
								next[i] = { ...x, received: true };
								marked = true;
								break;
							}
						}
						return marked ? next : m;
					});
				},
				'agent:action': (event) => {
					const data = event.payload;
					const tid = data.task_id;
					updateModelState('tool');
					const toolMsgId = toolId(
						tid,
						data.step_number,
						data.run_id,
						data.tool_call_id || data.tool_name,
					);
					const reasoningId = stepId('reasoning', tid, data.step_number, data.run_id);
					const thoughtId = stepId('thought', tid, data.step_number, data.run_id);
					pruneSeq(reasoningId);
					pruneSeq(thoughtId);
					if (data.silent) {
						// Silent tool: no card is shown, but the preceding text
						// must still be finalized so it is inserted immediately.
						updateTaskMessages(tid, (m) =>
							finalizeStreamBlocks(m, reasoningId, thoughtId),
						);
						return;
					}
					updateTaskMessages(tid, (m) => {
						// Finalize any streaming reasoning and thought blocks —
						// a tool action means the text/reasoning phase is over.
						// Clearing `segmented` drops straggler chunks that flush
						// out of the batcher after this event.
						const fixed = finalizeStreamBlocks(m, reasoningId, thoughtId);
						const existing = fixed.find((x) => x.id === toolMsgId);
						if (existing) return fixed;
						return [
							...fixed,
							newToolMessage({
								id: toolMsgId,
								stepNumber: data.step_number,
								toolName: data.tool_name,
								time: new Date().toLocaleTimeString(),
								streaming: true,
							}),
						];
					});
				},
				'agent:observation': (event) => {
					const data = event.payload;
					if (data.silent) return;
					const tid = data.task_id;
					updateModelState('streaming');
					const toolMsgId = toolId(
						tid,
						data.step_number,
						data.run_id,
						data.tool_call_id || data.tool_name,
					);
					updateTaskMessages(tid, (m) => {
						const idx = m.findIndex((x) => x.id === toolMsgId);
						const msg = newToolMessage({
							id: toolMsgId,
							stepNumber: data.step_number,
							toolName: data.tool_name,
							content: data.observation,
							askOptions: data.ask_options || [],
						});
						if (idx >= 0) {
							// Preserve the fields set by the action handler (e.g. the
							// bubble's timestamp) — only overwrite the observation
							// content and related fields.
							const next = [...m];
							next[idx] = { ...next[idx], ...msg, streaming: false };
							return next;
						}
						return [...m, msg];
					});
				},
				'confirm:requested': (event) => {
					const data = event.payload;
					if (data.task_id && activeTaskId && data.task_id !== activeTaskId) return;
					// If a confirmation is already pending, auto-reject the previous
					// one so the backend doesn't wait forever for a resolve_confirmation
					// that the user will never see.
					if (confirmDialog.stepId) {
						invoke('resolve_confirmation', {
							stepId: confirmDialog.stepId,
							confirmed: false,
							trustSession: false,
						}).catch(() => {});
					}
					confirmDialog = {
						stepId: data.step_id,
						toolName: data.tool_name,
						taskId: data.task_id,
						riskLevel: data.risk_level || 'medium',
					};
				},
				// Token usage / cost stats — emitted after every LLM step.
				'agent:usage': (event) => {
					const d = event.payload || {};
					if (!d.task_id) return;
					updateTaskTokenStats(d.task_id, {
						promptTokens: d.prompt_tokens || 0,
						completionTokens: d.completion_tokens || 0,
						totalTokens: d.total_tokens || 0,
						cumulativePromptTokens: d.cumulative_prompt_tokens || 0,
						cumulativeCompletionTokens: d.cumulative_completion_tokens || 0,
						cumulativeTotalTokens: d.cumulative_total_tokens || 0,
						costUsd: d.cost_usd ?? null,
						cumulativeCostUsd: d.cumulative_cost_usd ?? null,
						contextWindow: d.context_window ?? null,
						model: d.model ?? null,
						// A real usage event supersedes any restored estimate.
						estimated: false,
					});
				},
				// Context compaction notice — summarize a portion of the history.
				'agent:compaction': (event) => {
					const d = event.payload || {};
					const before = formatTokenCount(d.tokens_before || 0);
					const after = formatTokenCount(d.tokens_after || 0);
					addNotification(`上下文压缩：${before} → ${after} tokens`, 'info', 2500);
				},
			},
			{ tag: '+page' },
		);
		eventRegistrations = registrations;
		const readyP = registrations.ready;

		// Load the current default model for the toolbar model switcher and
		// populate the menu with models discovered from the default provider's
		// `/models` endpoint, mirroring the settings page behavior. Empty
		// api_key falls back to the stored key via the role name, and
		// discovery is skipped when no base URL is set. Fire-and-forget so it
		// never delays the conversation render.
		invoke('get_settings')
			.then((s) => {
				const dm = s?.llm?.default_model;
				if (dm?.model_name) {
					currentModelId = dm.model_name;
					currentModelName = dm.model_name;
				}
				currentEffort = dm?.reasoning_effort || '';
				currentWebSearch = dm?.web_search || 'off';
				if (s?.hotkey?.key_binding) {
					hotkeyBinding = s.hotkey.key_binding;
				}
				if (dm?.base_url) {
					ensureDefaultModelOptions(dm.base_url);
				}
			})
			.catch((e) => {
				logger.warn('+page', 'get_settings error', e);
			});

		// Load the task list and auto-restore the last conversation in
		// parallel; the conversation renders as soon as its data arrives,
		// without waiting for `reopen_task` (a second IPC round-trip that
		// only makes the task resumable for follow-up messages).
		const tasksP = loadTasks();
		const restoreP = restoreLastConversation(reviewTarget);

		await Promise.all([tasksP, restoreP, readyP]);

		// Conversation just opened (history review or auto-restore): scroll to
		// the real bottom, forcing the estimated content-visibility heights to
		// render first (see scrollToBottomAfterOpen).
		if (activeTaskId) {
			await tick();
			scrollToBottomAfterOpen();
		}

		if (browser) {
			window.addEventListener('click', handleWindowClick);
			window.addEventListener('contextmenu', handleWindowContextMenu);
		}
	});

	onDestroy(() => {
		dead = true;
		eventRegistrations?.dispose();
		if (browser) {
			window.removeEventListener('click', handleWindowClick);
			window.removeEventListener('contextmenu', handleWindowContextMenu);
		}
	});

	// Tracks the most recent loadTasks() invocation so the auto-restore can
	// order its decision after the task list without duplicating the
	// stale-pointer cleanup. Never rejects (errors are handled in loadTasks).
	let loadTasksSettled = Promise.resolve();

	async function loadTasks() {
		const seq = ++loadTasksSeq;
		const run = (async () => {
			const result = await invoke('get_tasks');
			// Stale response guard: a newer loadTasks call superseded this one.
			if (seq !== loadTasksSeq) return;
			if (result && result.tasks) {
				tasks = result.tasks;
				taskStore.set(tasks);
				// The active task can be ended (removed from the executor) while
				// this page is open — e.g. a follow-up message targeting a
				// terminal task is dropped server-side. Drop the stale pointer
				// so the next message starts a new task instead of hitting the
				// same terminal branch again.
				if (activeTaskId && !tasks.some((t) => t.id === activeTaskId)) {
					activeTaskId = null;
					activeTaskIdStore.set(null);
				}
				if (!activeTaskId && !suppressAutoTask) {
					const firstActive = tasks.find(
						(t) =>
							t.status === 'running' ||
							t.status === 'pending' ||
							t.status === 'paused',
					);
					if (firstActive) {
						activeTaskId = firstActive.id;
					}
				}
			}
		})().catch((e) => {
			addNotification(`加载任务列表失败: ${e}`, 'error', 3000);
		});
		loadTasksSettled = run;
		return run;
	}

	// Auto-restore the last conversation from a previous run so reopening
	// the app shows where you left off. Skipped when a review target is
	// pending, a task is already active, or the user explicitly started a
	// fresh conversation (新对话) and no new task has been created since.
	// Messages render as soon as `get_last_conversation` returns; the
	// follow-up `reopen_task` (which only lets follow-up messages continue
	// this task instead of being dropped as a terminal-task supplement) runs
	// afterwards without blocking the UI.
	async function restoreLastConversation(reviewTarget) {
		if (reviewTarget || (browser && localStorage.getItem('haven.no_auto_restore'))) return;
		// Wait for the task list first so the stale-activeTaskId check below
		// sees the real list (matches the previous sequential ordering) and a
		// running/paused task auto-assigned by loadTasks wins over the restore.
		await loadTasksSettled;
		if (activeTaskId && !tasks.some((t) => t.id === activeTaskId)) {
			activeTaskId = null;
			activeTaskIdStore.set(null);
		}
		if (activeTaskId) return;
		let last;
		try {
			last = await invoke('get_last_conversation');
		} catch (e) {
			logger.warn('+page', 'auto-restore conversation error', e);
			return;
		}
		// A task event or a later loadTasks auto-assigned one meanwhile —
		// don't clobber the live task with the restored conversation.
		if (!last?.task || activeTaskId) return;
		const wasError = last.task.status === 'error' || last.task.status === 'failed';
		updateTaskMessages(last.task.id, () => buildReviewMessages(last));
		restoreTaskTokenStats(last.task.id, last.usage, last.usage_estimated);
		activeTaskId = last.task.id;
		activeTaskIdStore.set(activeTaskId);
		if (wasError) {
			taskErrorId = last.task.id;
			activeTaskError = true;
		}
		try {
			await invoke('reopen_task', { taskId: last.task.id });
		} catch (e) {
			logger.warn('+page', 'reopen_task error', e);
		}
		await loadTasks();
	}

	// Deliver a user message to the backend. Shared by the normal send
	// button and the queued follow-up flush (which sends a stashed message
	// once the agent's current output completes).
	// The agent's ask questions are "awaiting" only while the task is paused
	// for the user's reply. Clear that state whenever the task resumes (the
	// user answered — by quick reply, typing, or voice) or its turn ends
	// (completed/error), so the "等待你的回答" indicator doesn't linger on
	// answered or abandoned questions. The `resolved` label is cleared too:
	// once the task resumes, any locally-chosen quick-reply answer that was
	// NOT part of the submitted message (e.g. the user typed their own reply
	// instead) must not keep displaying as "已选择/已忽略" — the submitted
	// user bubble is the record of what was actually sent.
	function clearAskAwaiting(taskId) {
		updateTaskMessages(taskId, (m) =>
			m.map((x) => (x.type === 'ask' ? { ...x, awaiting: false, resolved: null } : x)),
		);
		// A resume/end also invalidates any locally-chosen quick-reply answers
		// for the pending batch, so a later batch never inherits stale ones.
		resolvedAskIds.delete(taskId);
	}

	async function submitMessage(text, images, files) {
		try {
			const result = await submitTranscript(text, { images, files });
			if (result && result.TaskCreated) {
				activeTaskId = result.TaskCreated;
				activeTaskIdStore.set(activeTaskId);
				suppressAutoTask = false;
			}
			loadTasks();
		} catch (e) {
			addNotification(`发送失败: ${e}`, 'error', 5000);
		}
	}

	// Quick-reply answers / ignores chosen for the CURRENT batch of pending
	// ask questions, per task. When the agent asks several questions in one
	// batch (multiple `ask` calls in a single step), the task must stay
	// paused until every question is resolved — answering only one would
	// resume the task and silently discard the others. Once all are answered
	// or ignored, a single composed reply is submitted. Typing a message in
	// the input box bypasses this and resumes immediately.
	let resolvedAskIds = new Map(); // taskId -> Set<msgId>

	// Mark one pending ask card as resolved (answered via quick reply or
	// ignored) and submit the composed answers once the batch is complete.
	function resolveAsk(msgId, resolved) {
		if (!activeTaskId || !msgId) return;
		updateTaskMessages(activeTaskId, (m) =>
			m.map((x) =>
				x.id === msgId && x.type === 'ask' && x.awaiting
					? { ...x, awaiting: false, resolved }
					: x,
			),
		);
		const ids = resolvedAskIds.get(activeTaskId) || new Set();
		ids.add(msgId);
		resolvedAskIds.set(activeTaskId, ids);
		const remaining = (get(taskMessagesStore)[activeTaskId] || []).filter(
			(x) => x.type === 'ask' && x.awaiting,
		);
		if (remaining.length === 0) {
			const submitted = resolvedAskIds.get(activeTaskId);
			resolvedAskIds.delete(activeTaskId);
			submitAskAnswers(activeTaskId, submitted);
		}
	}

	// Compose all answers chosen for the resolved batch into a single user
	// message and deliver it, which resumes the paused task. A single
	// question keeps the raw answer; multiple questions quote each one so the
	// model can map answers back to its questions. Ignored questions are
	// marked as 忽略.
	function submitAskAnswers(taskId, resolvedIds) {
		if (!resolvedIds || resolvedIds.size === 0) return;
		const asks = (get(taskMessagesStore)[taskId] || []).filter(
			(x) => x.type === 'ask' && x.resolved && resolvedIds.has(x.id),
		);
		if (asks.length === 0) return;
		const single = asks.length === 1;
		const text = asks
			.map((x, i) => {
				const answer = x.resolved.ignored ? '忽略' : x.resolved.answer;
				return single ? answer : `关于「${x.content || `问题 ${i + 1}`}」：${answer}`;
			})
			.join('\n');
		autoFollow = true;
		submitMessage(text, []);
	}

	// The agent asked a question and offered quick-reply buttons. The answer
	// marks that question as resolved; the task resumes only when every
	// pending question in the batch is answered or ignored (see resolveAsk).
	function handleQuickReply(msgId, answer) {
		if (!activeTaskId || !answer) return;
		resolveAsk(msgId, { answer });
	}

	// The user chooses not to answer a pending question; counting as a
	// resolution so the batch can resume once all questions are handled.
	function handleIgnoreAsk(msgId) {
		if (!activeTaskId) return;
		resolveAsk(msgId, { ignored: true });
	}

	function handleSubmit() {
		const text = transcriptInput.trim();
		const images = pendingImages;
		const files = pendingFiles;
		if (!text && images.length === 0 && files.length === 0) return;
		transcriptInput = '';
		pendingImages = [];
		pendingFiles = [];
		autoFollow = true;

		submitMessage(text, images, files);
	}

	function handleKeydown(e) {
		if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
			e.preventDefault();
			handleSubmit();
		}
	}

	// Auto-grow the input to fit its content. While the content is a single
	// line, the vertical padding is balanced so the text renders centered
	// (matching the placeholder); multi-line content uses a fixed padding.
	const CHAT_INPUT_MIN_H = 44;
	const CHAT_INPUT_BASE_PAD = 8;
	const CHAT_INPUT_LINE_H = 20.3; // 14px font-size × 1.45 line-height
	function autoGrowInput() {
		const el = transcriptTextarea;
		if (!el) return;
		el.style.height = 'auto';
		el.style.paddingTop = '';
		el.style.paddingBottom = '';
		const contentH = el.scrollHeight;
		const singleLine = contentH <= CHAT_INPUT_MIN_H;
		el.style.height = Math.max(CHAT_INPUT_MIN_H, contentH) + 'px';
		if (singleLine) {
			// Balance the vertical padding against the inner height (border
			// excluded) so the single line of text sits exactly centered.
			const innerH = el.clientHeight;
			const totalPad = Math.max(0, innerH - CHAT_INPUT_LINE_H);
			const pad = Math.floor(totalPad / 2);
			el.style.paddingTop = pad + 'px';
			el.style.paddingBottom = totalPad - pad + 'px';
			el.style.setProperty('--chat-pad', pad + 'px');
		} else {
			el.style.setProperty('--chat-pad', CHAT_INPUT_BASE_PAD + 'px');
		}
	}
	$effect(() => {
		transcriptInput;
		transcriptTextarea;
		if (browser) autoGrowInput();
	});

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
		onClose={() => {
			if (!branchLoading)
				branchDialog = { open: false, stepNumber: null, role: '', content: '', msgId: '' };
		}}
	/>

	<!-- Right-click context menu -->
	{#if ctxMenu.open}
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<div class="ctx-menu" style="left: {ctxMenu.x}px; top: {ctxMenu.y}px;">
			<button class="ctx-item" onclick={handleCtxRollback}>
				<svg
					width="16"
					height="16"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
					><polyline points="1 4 1 10 7 10" /><path
						d="M3.51 15a9 9 0 1 0 2.13-9.36L1 10"
					/></svg
				>
				回退到此消息
			</button>
			<button class="ctx-item" onclick={handleCtxBranch}>
				<svg
					width="16"
					height="16"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
					><circle cx="6" cy="6" r="3" /><circle cx="6" cy="18" r="3" /><path
						d="M6 9v6"
					/><path d="M18 9h-6a4 4 0 0 0-4 4v4" /><circle cx="18" cy="6" r="3" /></svg
				>
				创建分支
			</button>
			<button class="ctx-item" onclick={handleCtxCopy}>
				<svg
					width="16"
					height="16"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
					><rect x="9" y="9" width="13" height="13" rx="2" /><path
						d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"
					/></svg
				>
				复制
			</button>
		</div>
	{/if}

	<div class="messages-wrap">
		<div class="messages-area" bind:this={messagesEl} onscroll={onScroll}>
			{#if messages.length === 0}
				<div class="welcome" in:fly={{ y: 12, duration: 330 }}>
					<Logo size={48} />
					<h2>Haven</h2>
					<p>PC 语音助手 · 按 {hotkeyBinding} 开始录音，或直接输入指令</p>
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
							attachments={msg.attachments}
							options={msg.options ?? []}
							awaiting={msg.awaiting ?? false}
							received={msg.received ?? false}
							resolved={msg.resolved ?? null}
							onContextMenu={handleContextMenu}
							onQuickReply={handleQuickReply}
							onIgnore={handleIgnoreAsk}
						/>
					{/each}
				</div>
			{/if}
			{#if activeTaskError}
				<div class="continue-banner" in:fly={{ y: 8, duration: 300 }}>
					<button
						class="md-btn md-btn--filled continue-btn"
						onclick={handleContinue}
						type="button"
					>
						<svg
							width="16"
							height="16"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"><polygon points="5 3 19 12 5 21 5 3" /></svg
						>
						继续生成
					</button>
				</div>
			{/if}
		</div>
		{#if !autoFollow && messages.length > 0}
			<button
				class="jump-bottom"
				onclick={jumpToBottom}
				aria-label="返回底部"
				title="返回底部"
				type="button"
			>
				<svg
					width="18"
					height="18"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
					stroke-linecap="round"
					stroke-linejoin="round"
					><path d="M12 5v14" /><polyline points="19 12 12 19 5 12" /></svg
				>
			</button>
		{/if}
	</div>

	<div class="input-area">
		{#if pendingFiles.length > 0}
			<div class="file-preview-row">
				{#each pendingFiles as file, i (file.filename + i)}
					<div class="file-preview">
						<svg
							class="file-preview-icon"
							width="18"
							height="18"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
							stroke-linecap="round"
							stroke-linejoin="round"
							aria-hidden="true"
						>
							<path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
							<polyline points="14 2 14 8 20 8" />
						</svg>
						<div class="file-preview-info">
							<span class="file-preview-name">{file.filename}</span>
							<span class="file-preview-size">{formatFileSize(file.size)}</span>
						</div>
						<button
							class="file-preview-remove"
							onclick={() => removePendingFile(i)}
							aria-label="移除文件"
							title="移除文件"
							type="button">&times;</button
						>
					</div>
				{/each}
			</div>
		{/if}
		{#if pendingImages.length > 0}
			<div class="image-preview-row">
				{#each pendingImages as img, i (img.data + i)}
					<div class="image-preview">
						<img src={imageDataUrl(img)} alt="待发送图片" />
						<button
							class="image-preview-remove"
							onclick={() => removePendingImage(i)}
							aria-label="移除图片"
							type="button">&times;</button
						>
					</div>
				{/each}
			</div>
		{/if}
		<div class="input-row">
			<textarea
				bind:this={transcriptTextarea}
				rows="1"
				placeholder={activeTaskId
					? '追加指令，Enter 发送，Shift+Enter 换行'
					: `输入指令，Enter 发送，或按 ${hotkeyBinding} 录音`}
				bind:value={transcriptInput}
				onkeydown={handleKeydown}
				onpaste={handlePaste}
				class="md-input chat-input"
				autocomplete="off"
			></textarea>
		</div>
		<div class="toolbar-row">
			<div class="toolbar-left">
				<div class="task-switch">
					<button
						class="md-btn md-btn--outlined task-switch-btn"
						onclick={() => {
							if (showTaskMenu) {
								taskMenuOpen = !taskMenuOpen;
							} else {
								newTask();
							}
						}}
						title={showTaskMenu ? '切换并行任务或开始新任务' : '开始一个新任务'}
						type="button"
					>
						<svg
							width="20"
							height="20"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
							stroke-linecap="round"
							stroke-linejoin="round"
							><line x1="12" y1="5" x2="12" y2="19" /><line
								x1="5"
								y1="12"
								x2="19"
								y2="12"
							/></svg
						>
						{#if showTaskMenu}
							<svg
								class="task-switch-caret"
								width="16"
								height="16"
								viewBox="0 0 24 24"
								fill="none"
								stroke="currentColor"
								stroke-width="2"
								stroke-linecap="round"
								stroke-linejoin="round"><polyline points="6 9 12 15 18 9" /></svg
							>
						{/if}
					</button>
					{#if taskMenuOpen}
						<div class="task-menu">
							<div class="task-menu-title">正在执行的任务</div>
							{#each parallelTasks as t}
								<button
									class="task-menu-item"
									class:selected={t.id === activeTaskId}
									onclick={() => switchToTask(t.id)}
									type="button"
								>
									<span class="task-menu-item-title"
										>{t.title || t.id.slice(0, 8)}</span
									>
									<span
										class="task-menu-item-status"
										class:running={t.status === 'running'}
									>
										{t.status === 'running' ? '运行中' : '等待中'}
									</span>
								</button>
							{/each}
							<div class="task-menu-divider"></div>
							<button
								class="task-menu-item task-menu-new"
								onclick={() => newTask()}
								type="button"
							>
								<svg
									width="16"
									height="16"
									viewBox="0 0 24 24"
									fill="none"
									stroke="currentColor"
									stroke-width="2"
									stroke-linecap="round"
									stroke-linejoin="round"
									><line x1="12" y1="5" x2="12" y2="19" /><line
										x1="5"
										y1="12"
										x2="19"
										y2="12"
									/></svg
								>
								新建任务
							</button>
						</div>
					{/if}
				</div>
				{#if activeTaskId}
					<button
						class="md-btn md-btn--outlined end-task-btn"
						onclick={endTask}
						aria-label="结束任务"
						title="结束当前任务"
						type="button"
					>
						<svg
							width="18"
							height="18"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
							stroke-linecap="round"
							stroke-linejoin="round"
							><rect x="6" y="6" width="12" height="12" rx="2" /></svg
						>
					</button>
				{/if}
				<div
					class="token-stats"
					class:active={!!tokenStats}
					title={tokenStats ? buildTokenTooltip(tokenStats) : tokenStatsHint}
				>
					{#if tokenStats}
						<svg
							class="token-icon"
							width="16"
							height="16"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
							stroke-linecap="round"
							stroke-linejoin="round"
							aria-hidden="true"
						>
							<path d="M4 6h16M4 12h10M4 18h16" />
						</svg>
						<div class="token-text">
							<span class="token-cumulative"
								>{tokenStats.estimated ? '约 ' : ''}{formatTokenCount(
									tokenStats.cumulativeTotalTokens,
								)}</span
							>
							{#if !tokenStats.estimated && tokenStats.cumulativeCostUsd != null}
								<span class="token-cost"
									>· {formatCostUsd(tokenStats.cumulativeCostUsd)}</span
								>
							{/if}
						</div>
						{#if contextBudget}
							<div
								class="token-budget"
								class:warn={contextBudget.ratio >= 0.75}
								class:danger={contextBudget.ratio >= 0.9}
								aria-label={`上下文使用 ${(contextBudget.ratio * 100).toFixed(0)}%`}
							>
								<div
									class="token-budget-fill"
									style="width: {(contextBudget.ratio * 100).toFixed(1)}%"
								></div>
							</div>
						{/if}
					{:else}
						<svg
							class="token-icon"
							width="16"
							height="16"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
							stroke-linecap="round"
							stroke-linejoin="round"
							aria-hidden="true"
						>
							<path d="M4 6h16M4 12h10M4 18h16" />
						</svg>
						<span class="token-text token-idle">—</span>
					{/if}
				</div>
			</div>
			<div class="toolbar-right">
				<button
					class="md-icon-button file-btn"
					onclick={() => attachFileInput?.click()}
					aria-label="添加附件"
					title="添加图片或文件"
					type="button"
				>
					<svg
						width="20"
						height="20"
						viewBox="0 0 24 24"
						fill="none"
						stroke="currentColor"
						stroke-width="2"
						stroke-linecap="round"
						stroke-linejoin="round"
					>
						<path
							d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48"
						/>
					</svg>
				</button>
				<input
					hidden
					type="file"
					multiple
					bind:this={attachFileInput}
					onchange={handleAttachSelect}
				/>
				<button
					class="md-icon-button record-btn"
					class:recording={recordingState.isRecording}
					onclick={handleRecordClick}
					aria-label={recordingState.isRecording ? '停止录音' : '开始录音'}
					title={recordingState.isRecording ? '停止录音' : '开始录音'}
					type="button"
				>
					{#if recordingState.isRecording}
						<svg
							width="20"
							height="20"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
							stroke-linecap="round"
							stroke-linejoin="round"
							><rect x="6" y="6" width="12" height="12" rx="2" /></svg
						>
					{:else}
						<svg
							width="20"
							height="20"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
							stroke-linecap="round"
							stroke-linejoin="round"
							><path d="M12 2a3 3 0 0 0-3 3v6a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3z" /><path
								d="M19 10v1a7 7 0 0 1-14 0v-1"
							/><line x1="12" y1="19" x2="12" y2="22" /></svg
						>
					{/if}
				</button>
				<div class="model-switch">
					<button
						class="md-icon-button model-switch-btn"
						onclick={() => (modelMenuOpen = !modelMenuOpen)}
						title={`切换默认模型${currentModelName ? `：${currentModelName}` : ''}`}
						aria-label="切换默认模型"
						type="button"
					>
						<svg
							width="20"
							height="20"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
							stroke-linecap="round"
							stroke-linejoin="round"
							><rect x="5" y="5" width="14" height="14" rx="2" /><rect
								x="9.5"
								y="9.5"
								width="5"
								height="5"
							/></svg
						>
					</button>
					{#if modelMenuOpen}
						<div class="model-menu">
							<div class="model-menu-title">切换默认模型</div>
							{#each modelOptions as m}
								<button
									class="model-item"
									class:selected={m.id === currentModelId}
									onclick={() => handleModelSelect(m)}
									type="button"
								>
									<span class="model-item-name">{m.name}</span>
									<span class="model-item-provider">{m.provider}</span>
								</button>
							{/each}
							<div class="model-menu-divider"></div>
							<div class="model-menu-title">思考强度</div>
							<div class="effort-row">
								{#each effortOptions as opt}
									<button
										class="effort-item"
										class:selected={currentEffort === opt.value}
										onclick={() => handleEffortSelect(opt.value)}
										type="button">{opt.label}</button
									>
								{/each}
							</div>
							<div class="model-menu-divider"></div>
							<div class="model-menu-title">联网搜索</div>
							<div class="effort-row">
								{#each webSearchOptions as opt}
									<button
										class="effort-item"
										class:selected={currentWebSearch === opt.value}
										onclick={() => handleWebSearchSelect(opt.value)}
										type="button">{opt.label}</button
									>
								{/each}
							</div>
						</div>
					{/if}
				</div>
				<button
					class="md-icon-button send-btn"
					class:stop-mode={stopMode}
					onclick={stopMode ? () => endTask() : handleSubmit}
					disabled={!hasInput && !isGenerating && !taskRunning}
					aria-label={hasInput ? '发送' : stopMode ? '停止任务' : '发送'}
					title={hasInput ? '发送' : stopMode ? '停止任务' : '发送'}
					type="button"
				>
					{#if hasInput}
						<svg
							width="20"
							height="20"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
							stroke-linecap="round"
							stroke-linejoin="round"
						>
							<line x1="12" y1="19" x2="12" y2="5" />
							<polyline points="5 12 12 5 19 12" />
						</svg>
					{:else if stopMode}
						<svg
							width="20"
							height="20"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
							stroke-linecap="round"
							stroke-linejoin="round"
							><rect x="6" y="6" width="12" height="12" rx="2" /></svg
						>
					{:else}
						<svg
							width="20"
							height="20"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
							stroke-linecap="round"
							stroke-linejoin="round"
						>
							<line x1="12" y1="19" x2="12" y2="5" />
							<polyline points="5 12 12 5 19 12" />
						</svg>
					{/if}
				</button>
			</div>
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
	.messages-wrap {
		position: relative;
		flex: 1;
		min-height: 0;
		display: flex;
		flex-direction: column;
		/* Chat content has its own narrower reading-friendly cap; the
		 * layout shell handles the wider-page case so we only need to
		 * keep messages from getting too narrow on small viewports. */
		max-width: clamp(600px, 92vw, 800px);
		margin: 0 auto;
		width: 100%;
	}
	.messages-area {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		padding: var(--md-sys-space-md);
	}
	.jump-bottom {
		position: absolute;
		right: var(--md-sys-space-md);
		bottom: var(--md-sys-space-sm);
		width: 36px;
		height: 36px;
		border: none;
		border-radius: 50%;
		display: flex;
		align-items: center;
		justify-content: center;
		background: var(--md-sys-color-primary-container);
		color: var(--md-sys-color-on-primary-container);
		cursor: pointer;
		box-shadow: var(--md-sys-elevation-2);
		transition: background var(--md-sys-motion-duration-short)
			var(--md-sys-motion-easing-standard);
		z-index: 5;
	}
	.jump-bottom:hover {
		background: var(--md-sys-color-primary);
		color: var(--md-sys-color-on-primary);
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
		background: var(--md-sys-color-surface-container-low);
		padding: var(--md-sys-space-md) var(--md-sys-space-lg) var(--md-sys-space-md);
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
		flex-shrink: 0;
		max-width: clamp(600px, 92vw, 800px);
		margin: 0 auto;
		width: 100%;
	}

	.image-preview-row {
		display: flex;
		flex-wrap: wrap;
		gap: var(--md-sys-space-sm);
	}
	.image-preview {
		position: relative;
		width: 64px;
		height: 64px;
		border-radius: var(--md-sys-shape-small);
		overflow: hidden;
		border: 1px solid var(--md-sys-color-outline-variant);
	}
	.image-preview img {
		width: 100%;
		height: 100%;
		object-fit: cover;
		display: block;
	}
	.image-preview-remove {
		position: absolute;
		top: 2px;
		right: 2px;
		width: 20px;
		height: 20px;
		border-radius: 50%;
		border: none;
		background: rgba(0, 0, 0, 0.6);
		color: #fff;
		font-size: 13px;
		line-height: 1;
		cursor: pointer;
		display: flex;
		align-items: center;
		justify-content: center;
	}

	.file-preview-row {
		display: flex;
		flex-wrap: wrap;
		gap: var(--md-sys-space-sm);
	}
	.file-preview {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		max-width: 260px;
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		border: 1px solid var(--md-sys-color-outline-variant);
		background: var(--md-sys-color-surface-container-high);
	}
	.file-preview-icon {
		flex-shrink: 0;
		color: var(--md-sys-color-on-surface-variant);
	}
	.file-preview-info {
		min-width: 0;
		display: flex;
		flex-direction: column;
	}
	.file-preview-name {
		font-size: 12px;
		color: var(--md-sys-color-on-surface);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		max-width: 170px;
	}
	.file-preview-size {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
	}
	.file-preview-remove {
		width: 20px;
		height: 20px;
		margin-left: auto;
		border-radius: 50%;
		border: none;
		flex-shrink: 0;
		background: var(--md-sys-color-surface-container-highest);
		color: var(--md-sys-color-on-surface-variant);
		font-size: 13px;
		line-height: 1;
		cursor: pointer;
		display: flex;
		align-items: center;
		justify-content: center;
	}
	.file-preview-remove:hover {
		background: var(--md-sys-color-error-container);
		color: var(--md-sys-color-on-error-container);
	}
	.file-btn {
		flex-shrink: 0;
	}

	.input-row {
		display: flex;
		gap: var(--md-sys-space-xs);
		align-items: flex-end;
	}
	.chat-input {
		--chat-pad: 8px;
		background: var(--md-sys-color-surface-container-high);
		border: 1px solid transparent;
		border-radius: var(--md-sys-shape-medium);
		min-height: 44px;
		height: auto;
		flex: 1;
		min-width: 0;
		padding: var(--chat-pad) var(--md-sys-space-md);
		resize: none;
		overflow-y: auto;
		line-height: 1.45;
		font-size: 14px;
	}
	.chat-input::placeholder {
		/* Placeholder line-height tracks the balanced padding so it stays
		   vertically centered exactly like the (balanced) input text. */
		line-height: calc(44px - 2 * var(--chat-pad) - 2px);
	}
	.chat-input:hover {
		border-color: var(--md-sys-color-outline-variant);
	}
	.chat-input:focus {
		border-color: var(--md-sys-color-primary);
		border-width: 2px;
		padding: var(--chat-pad) calc(var(--md-sys-space-md) - 1px);
	}

	.toolbar-row {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
	}
	.toolbar-left {
		flex: 0 0 auto;
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
	}
	.toolbar-right {
		flex: 0 0 auto;
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		margin-left: auto;
	}
	.toolbar-row :global(.md-btn) {
		height: 40px;
		padding: 0 var(--md-sys-space-md);
		font-size: 13px;
	}
	.toolbar-row :global(.md-icon-button) {
		width: 40px;
		height: 40px;
		min-width: 40px;
		min-height: 40px;
		padding: 0;
	}
	.record-btn {
		flex-shrink: 0;
	}
	.record-btn.recording {
		--_ib-fg: var(--md-sys-color-error);
		--_ib-bg: var(--md-sys-color-error-container);
	}
	.task-switch {
		position: relative;
		flex-shrink: 0;
	}
	.task-switch-btn {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
	}
	.task-switch-caret {
		flex-shrink: 0;
	}
	.end-task-btn {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		flex-shrink: 0;
		color: var(--md-sys-color-error);
		border-color: var(--md-sys-color-error);
	}
	.end-task-btn:hover {
		background: var(--md-sys-color-error-container);
		border-color: var(--md-sys-color-error);
		color: var(--md-sys-color-on-error-container);
	}
	.task-menu {
		position: absolute;
		left: 0;
		bottom: calc(100% + 8px);
		z-index: 1000;
		min-width: 240px;
		max-width: 320px;
		max-height: 320px;
		overflow-y: auto;
		background: var(--md-sys-color-surface-container-high);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		padding: var(--md-sys-space-xs);
		box-shadow: var(--md-sys-elevation-2);
	}
	.task-menu-title {
		font-size: 11px;
		font-weight: 600;
		letter-spacing: 0.4px;
		text-transform: uppercase;
		color: var(--md-sys-color-on-surface-variant);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
	}
	.task-menu-item {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-sm);
		width: 100%;
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border: none;
		background: transparent;
		color: var(--md-sys-color-on-surface);
		font-size: 13px;
		font-family: inherit;
		cursor: pointer;
		border-radius: var(--md-sys-shape-small);
		transition: background var(--md-sys-motion-duration-fast)
			var(--md-sys-motion-easing-standard);
	}
	.task-menu-item:hover {
		background: var(--md-sys-color-surface-container-highest);
	}
	.task-menu-item.selected .task-menu-item-title {
		color: var(--md-sys-color-primary);
		font-weight: 600;
	}
	.task-menu-item-title {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.task-menu-item-status {
		flex-shrink: 0;
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
	}
	.task-menu-item-status.running {
		color: var(--md-sys-color-primary);
	}
	.task-menu-divider {
		height: 1px;
		background: var(--md-sys-color-outline-variant);
		margin: var(--md-sys-space-xs) 0;
	}
	.task-menu-new {
		justify-content: flex-start;
		gap: var(--md-sys-space-sm);
		color: var(--md-sys-color-primary);
		font-weight: 600;
	}
	.token-stats {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		min-width: 84px;
		height: 40px;
		padding: 0 var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-corner-medium, 8px);
		border: 1px solid var(--md-sys-color-outline-variant);
		background: var(--md-sys-color-surface-container, transparent);
		color: var(--md-sys-color-on-surface-variant);
		font-size: 12px;
		line-height: 1;
		flex-shrink: 0;
		transition: border-color var(--md-sys-motion-duration-short)
			var(--md-sys-motion-easing-standard);
	}
	.token-stats.active {
		border-color: var(--md-sys-color-primary);
	}
	.token-icon {
		opacity: 0.75;
		flex-shrink: 0;
	}
	.token-text {
		display: inline-flex;
		gap: 4px;
		align-items: baseline;
		font-variant-numeric: tabular-nums;
		white-space: nowrap;
	}
	.token-cumulative {
		font-weight: 600;
		color: var(--md-sys-color-on-surface);
	}
	.token-cost {
		opacity: 0.7;
	}
	.token-idle {
		opacity: 0.5;
	}
	.token-budget {
		position: relative;
		width: 36px;
		height: 4px;
		border-radius: 999px;
		background: var(--md-sys-color-surface-variant, rgba(0, 0, 0, 0.06));
		overflow: hidden;
		flex-shrink: 0;
	}
	.token-budget-fill {
		position: absolute;
		inset: 0 auto 0 0;
		background: var(--md-sys-color-primary);
		transition:
			width var(--md-sys-motion-duration-medium) var(--md-sys-motion-easing-standard),
			background var(--md-sys-motion-duration-medium) var(--md-sys-motion-easing-standard);
	}
	.token-budget.warn .token-budget-fill {
		background: #c97a00;
	}
	.token-budget.danger .token-budget-fill {
		background: var(--md-sys-color-error, #b3261e);
	}
	.model-switch {
		position: relative;
		flex-shrink: 0;
	}
	.model-menu {
		position: absolute;
		right: 0;
		bottom: calc(100% + 8px);
		z-index: 1000;
		min-width: 240px;
		max-height: 320px;
		overflow-y: auto;
		background: var(--md-sys-color-surface-container-high);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		padding: var(--md-sys-space-xs);
		box-shadow: var(--md-sys-elevation-2);
	}
	.model-menu-title {
		font-size: 11px;
		font-weight: 600;
		letter-spacing: 0.4px;
		text-transform: uppercase;
		color: var(--md-sys-color-on-surface-variant);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
	}
	.model-item {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-sm);
		width: 100%;
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border: none;
		background: transparent;
		color: var(--md-sys-color-on-surface);
		font-size: 13px;
		font-family: inherit;
		cursor: pointer;
		border-radius: var(--md-sys-shape-small);
		transition: background var(--md-sys-motion-duration-fast)
			var(--md-sys-motion-easing-standard);
	}
	.model-item:hover {
		background: var(--md-sys-color-surface-container-highest);
	}
	.model-item.selected .model-item-name {
		color: var(--md-sys-color-primary);
		font-weight: 600;
	}
	.model-item-provider {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
	}
	.model-menu-divider {
		height: 1px;
		background: var(--md-sys-color-outline-variant);
		margin: var(--md-sys-space-xs) 0;
	}
	.effort-row {
		display: flex;
		gap: var(--md-sys-space-xs);
		padding: 0 var(--md-sys-space-md) var(--md-sys-space-sm);
	}
	.effort-item {
		flex: 1;
		height: 32px;
		border: 1px solid var(--md-sys-color-outline);
		border-radius: var(--md-sys-shape-small);
		background: transparent;
		color: var(--md-sys-color-on-surface-variant);
		font-size: 12px;
		font-weight: 600;
		font-family: inherit;
		cursor: pointer;
		transition:
			background-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			border-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}
	.effort-item:hover {
		border-color: var(--md-sys-color-primary);
	}
	.effort-item.selected {
		border-color: var(--md-sys-color-primary);
		background: var(--md-sys-color-primary);
		color: var(--md-sys-color-on-primary);
	}
	.send-btn {
		flex-shrink: 0;
		--_ib-fg: var(--md-sys-color-on-primary);
		--_ib-bg: var(--md-sys-color-primary);
		--_ib-state: var(--md-sys-color-on-primary);
	}
	.send-btn:hover {
		box-shadow: var(--md-sys-elevation-1);
	}
	.send-btn.stop-mode {
		--_ib-fg: var(--md-sys-color-on-error);
		--_ib-bg: var(--md-sys-color-error);
		--_ib-state: var(--md-sys-color-on-error);
	}
	.ctx-menu {
		position: fixed;
		z-index: 1000;
		background: var(--md-sys-color-surface-container-high);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		padding: var(--md-sys-space-xs);
		box-shadow: var(--md-sys-elevation-2);
		min-width: 160px;
	}
	.ctx-item {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		width: 100%;
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border: none;
		background: transparent;
		color: var(--md-sys-color-on-surface);
		font-size: 13px;
		font-family: inherit;
		cursor: pointer;
		border-radius: var(--md-sys-shape-small);
		transition: background var(--md-sys-motion-duration-fast)
			var(--md-sys-motion-easing-standard);
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
		justify-content: flex-start;
		gap: var(--md-sys-space-md);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		max-width: clamp(600px, 92vw, 800px);
		margin: 0 auto;
		width: 100%;
	}
	.continue-btn {
		gap: var(--md-sys-space-xs);
		font-size: 13px;
	}
</style>
