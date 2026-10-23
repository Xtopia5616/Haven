<script lang="ts">
	import { cubicOut } from 'svelte/easing';
	import { fly } from 'svelte/transition';
	import type {
		InputDeliveryModeInput,
		SessionStatus,
		SessionWaitingReason,
	} from '$lib/contracts/generatedCommands.ts';
	import { onDestroy, tick, untrack } from 'svelte';
	import type { Snippet } from 'svelte';
	import logger from '$lib/logger.ts';
	import { browser } from '$app/environment';
	import { addNotification } from '$lib/notificationStore.ts';
	import { recordingOverlayController } from '$lib/recordingOverlayController.ts';
	import { mediaDataUrl } from '$lib/mediaData.ts';
	import { reportError } from '$lib/errorHandling.ts';
	import { syncStore } from '$lib/syncStore.ts';
	import { openContextMenu } from '$lib/contextMenuStore.ts';
	import MaterialIconButton from '$lib/MaterialIconButton.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import type { SelectOption } from '$lib/selectOption.ts';
	import type { ReActExecutionPhase } from '$lib/reactExecutionPhaseStore.ts';
	import Icon from '$lib/Icon.svelte';
	import { copyText } from '$lib/clipboard.ts';
	import type { ContextMenuItem } from '$lib/contextMenuStore.ts';
	import type { ChatAttachmentPayload, ChatFileAttachment } from '$lib/chatAttachmentTypes.ts';
	import { DRAFT_SESSION_ID } from '$lib/sessionReducer.ts';
	import { publishWorkspaceStatusHint } from '$lib/workspaceStatusHintStore.ts';

	const DELIVERY_MODE_OPTIONS: SelectOption[] = [
		{ value: 'steering', label: '安全边界调整' },
		{ value: 'follow_up', label: '完成后追加' },
	];
	const ASK_DELIVERY_MODE_OPTIONS: SelectOption[] = [
		{ value: 'auto', label: '回答当前问题' },
		{ value: 'follow_up', label: '完成后追加' },
	];

	interface PendingChatImageAttachment extends ChatAttachmentPayload {
		previewKey: string;
	}

	interface PendingChatFileAttachment extends ChatFileAttachment {
		size: number;
		previewKey: string;
	}

	interface ComposerSubmitPayload {
		deliveryMode?: InputDeliveryModeInput;
		text: string;
		images: ChatAttachmentPayload[];
		files: ChatFileAttachment[];
	}

	interface TextSelection {
		start: number;
		end: number;
		text: string;
	}

	interface Props {
		activeSessionId?: string | null;
		hotkeyBinding?: string;
		isGenerating?: boolean;
		sessionRunning?: boolean;
		executionPhase?: ReActExecutionPhase;
		sessionStatus?: SessionStatus | null;
		sessionStatusLabel?: string;
		sessionWaitingReason?: SessionWaitingReason | null;
		interrupting?: boolean;
		askAwaiting?: boolean;
		askHasOptions?: boolean;
		confirmationAwaiting?: boolean;
		allowEmptySubmit?: boolean;
		onsubmit?: (payload: ComposerSubmitPayload) => void;
		onstop?: () => void;
		toolbarLeft?: Snippet;
		toolbarRight?: Snippet;
		maxImages?: number;
		maxImageBytes?: number;
		maxImageDim?: number;
		jpegQuality?: number;
		maxFiles?: number;
		maxFileBytes?: number;
	}

	let {
		activeSessionId = null,
		hotkeyBinding = 'Ctrl+Shift+Space',
		isGenerating = false,
		sessionRunning = false,
		executionPhase = 'idle',
		sessionStatus = null,
		sessionStatusLabel = '空闲',
		sessionWaitingReason = null,
		interrupting = false,
		// While an ask card is waiting, guide the user from the input itself.
		askAwaiting = false,
		askHasOptions = false,
		confirmationAwaiting = false,
		// When true, Enter may submit even with an empty draft (e.g. ask option
		// chips are selected and the page will compose the answer).
		allowEmptySubmit = false,
		onsubmit = undefined,
		onstop = undefined,
		toolbarLeft = undefined,
		toolbarRight = undefined,
		// Attachment & compression limits, driven by the settings "媒体"
		// page via [context_limits]; defaults mirror the backend config.
		maxImages = 4,
		maxImageBytes = 10 * 1024 * 1024,
		maxImageDim = 1568,
		jpegQuality = 0.85,
		maxFiles = 5,
		maxFileBytes = 20 * 1024 * 1024,
	}: Props = $props();

	// Pending image attachments (multimodal): [{ mediaType, data }] with data
	// holding base64 bytes (no data: prefix). Filled by paste / file picker,
	// sent along with the next message, cleared on submit.
	let pendingImages = $state<PendingChatImageAttachment[]>([]);

	// Pending non-image attachments: [{ media_type, data, filename, size }].
	// Ordinary files are persisted by the backend and handed to the agent as a
	// managed asset; audio/video keep their base64 payload for multimodal chat.
	let pendingFiles = $state<PendingChatFileAttachment[]>([]);
	let nextAttachmentPreviewKey = 0;

	function newAttachmentPreviewKey(): string {
		return `draft-attachment-${++nextAttachmentPreviewKey}`;
	}
	// Reserve slots while asynchronous reads are in flight so overlapping
	// picker/paste events cannot exceed the configured attachment limits.
	let pendingImageReads = 0;
	let pendingFileReads = 0;
	let pendingAttachmentReads = $state(0);
	// Single hidden picker for both images and files; the picked items are
	// split by type on selection (images -> pendingImages, rest -> pendingFiles).
	let attachFileInput = $state<HTMLInputElement | null>(null);

	// Read-only recording state mirror so the
	// toolbar mic button can toggle start/stop inline.
	let recordingState = $state({ isRecording: false });
	$effect(() =>
		syncStore(recordingOverlayController.state, (v) => {
			recordingState = v;
		}),
	);

	let deliveryMode = $state<Exclude<InputDeliveryModeInput, 'auto'>>('steering');
	let askDeliveryMode = $state<'auto' | 'follow_up'>('auto');
	let previousAskSessionId = $state<string | null>(null);
	$effect(() => {
		if (!askAwaiting) {
			previousAskSessionId = null;
			return;
		}
		if (previousAskSessionId !== activeSessionId) {
			previousAskSessionId = activeSessionId;
			askDeliveryMode = 'auto';
		}
	});
	let deliveryModeAvailable = $derived(sessionRunning || isGenerating || askAwaiting);
	let deliveryModeOptions = $derived(
		askAwaiting ? ASK_DELIVERY_MODE_OPTIONS : DELIVERY_MODE_OPTIONS,
	);
	let selectedDeliveryMode = $derived(askAwaiting ? askDeliveryMode : deliveryMode);
	let allowEmptyMessageSubmit = $derived(
		allowEmptySubmit && (!askAwaiting || askDeliveryMode === 'auto'),
	);
	let deliveryHint = $derived.by(() => {
		if (interrupting) return '正在停止当前输出；停止完成后再发送新消息。';
		if (askAwaiting) {
			return askDeliveryMode === 'follow_up'
				? '此消息会排队，不会回答或唤醒当前问题；当前所选答案选项也不会提交。请先回答问题，后续消息才会处理。'
				: '正在等待你的回答；发送内容和已选选项会作为当前问题的答案。';
		}
		if (confirmationAwaiting) {
			return '工具正在等待权限确认；此处发送的消息只会排队，确认前不会开始处理。';
		}
		if (sessionStatus === 'paused') {
			if (sessionStatusLabel === '本轮已完成') {
				return '当前工作已自然完成；发送消息会作为下一轮追加并继续会话。';
			}
			if (sessionWaitingReason === 'end_incomplete') {
				return '上一轮未完整结束；发送普通消息会排入后续并尝试恢复会话。';
			}
			if (sessionWaitingReason === 'step_budget') {
				return '本轮步骤预算已耗尽；发送普通消息会排入后续并恢复会话。';
			}
			if (sessionWaitingReason === 'user_interrupt') {
				return '会话已被中断；发送普通消息会排入后续并继续会话。';
			}
			return '会话已暂停；发送普通消息会排入后续，并尝试恢复会话。';
		}
		if (sessionStatus === 'completed' || sessionStatus === 'error') {
			return '此会话已结束；发送新消息会开始新的会话。';
		}
		if (!deliveryModeAvailable) return '';

		if (executionPhase === 'queued' || sessionStatus === 'pending') {
			return deliveryMode === 'follow_up'
				? '会话排队中；本条消息会在当前工作自然完成后追加。'
				: '会话排队中；本条消息会在会话启动后的安全边界接入。';
		}
		if (executionPhase === 'waiting_result') {
			return deliveryMode === 'follow_up'
				? '工具仍在执行；完成后追加会等整个工作自然结束，不会中断工具。'
				: '工具仍在执行；安全边界调整会等工具返回后接入，不会中断工具。';
		}
		if (executionPhase === 'waiting_response') {
			return deliveryMode === 'follow_up'
				? '当前交互尚未结束；完成后追加会继续等待，不会替代确认或问题回答。'
				: '当前交互尚未结束；安全边界调整会等待交互处理后再接入。';
		}
		if (deliveryMode === 'follow_up') {
			return '等待当前工作自然完成后追加；工具续轮、确认和 Ask 等待都不算完成。';
		}
		if (executionPhase === 'requesting' || executionPhase === 'generating') {
			return '当前模型请求结束后，在下一个安全边界接入；不会中断当前生成。';
		}
		return '将在下一个安全边界接入当前工作；不会中断正在执行的请求或工具。';
	});
	$effect(() => {
		publishWorkspaceStatusHint(deliveryHint || null);
	});
	onDestroy(() => publishWorkspaceStatusHint(null));
	let transcriptInput = $state('');
	let transcriptDraftSessionId = DRAFT_SESSION_ID;
	const cachedDrafts = new Map<string, string>();
	const MAX_CACHED_DRAFTS = 100;

	function cacheDraft(sessionId: string, text: string) {
		cachedDrafts.delete(sessionId);
		if (!text) return;
		cachedDrafts.set(sessionId, text);
		while (cachedDrafts.size > MAX_CACHED_DRAFTS) {
			const oldestSessionId = cachedDrafts.keys().next().value;
			if (oldestSessionId === undefined) break;
			cachedDrafts.delete(oldestSessionId);
		}
	}

	// Keep one text draft per session while this composer stays mounted.
	// The fresh-session slot is separate from every persisted session.
	$effect(() => {
		const nextSessionId = activeSessionId || DRAFT_SESSION_ID;
		const previous = untrack(() => ({
			sessionId: transcriptDraftSessionId,
			text: transcriptInput,
		}));
		if (nextSessionId === previous.sessionId) return;

		cacheDraft(previous.sessionId, previous.text);
		transcriptDraftSessionId = nextSessionId;
		transcriptInput = cachedDrafts.get(nextSessionId) ?? '';
		cachedDrafts.delete(nextSessionId);
	});

	let transcriptTextarea = $state<HTMLTextAreaElement | null>(null);

	const hasDraft = $derived(
		transcriptInput.trim().length > 0 || pendingImages.length > 0 || pendingFiles.length > 0,
	);
	// Treat selected Ask chips as submit-ready only while the input is routed
	// as an answer. A follow-up must contain its own content.
	const hasInput = $derived(hasDraft || allowEmptyMessageSubmit);
	// Steering becomes eligible at the next safe turn boundary. The active
	// model request or tool is never interrupted by this input route.
	// The merged send button becomes "interrupt output" only when there is no input
	// and the agent is actively working (generating output, a running/pending
	// session). With fresh input present, it always stays a send button.
	const stopMode = $derived(!hasInput && (interrupting || isGenerating || sessionRunning));
	const chatInputPlaceholder = $derived.by(() => {
		if (askAwaiting) {
			if (askDeliveryMode === 'follow_up') {
				return askHasOptions
					? '输入会排队为后续消息；不会提交为当前问题答案'
					: '输入后续消息；当前问题仍等待回答';
			}
			return askHasOptions
				? '从上方的选项中选择，或者在此处输入答案或补充，Enter 提交'
				: '在此处输入答案或补充，Enter 提交';
		}
		if (interrupting) return '当前正在停止；稍后可继续发送消息';
		if (confirmationAwaiting) return '工具等待权限确认；消息会排队至确认完成';
		if (sessionStatus === 'completed' || sessionStatus === 'error') {
			return '输入指令以开始新的会话';
		}
		return activeSessionId
			? `追加指令，Enter 发送，Shift+Enter 换行；按 ${hotkeyBinding} 录音`
			: `输入指令，Enter 发送，或按 ${hotkeyBinding} 录音`;
	});

	// Allow the host page to populate the draft box programmatically (e.g.
	// restoring a message after rollback) via `bind:this`.
	export function setDraft(text: string) {
		transcriptInput = text ?? '';
		tick().then(() => transcriptTextarea?.focus());
	}

	/** Append approved host-provided text while keeping the existing draft intact. */
	export function appendDraft(text: string) {
		if (!text) return;
		transcriptInput = transcriptInput ? `${transcriptInput}\n\n${text}` : text;
	}

	async function handleRecordClick() {
		try {
			await recordingOverlayController.toggleFromToolbar();
		} catch (e) {
			reportError(e, { context: 'Composer', message: '录音失败' });
		}
	}

	/** Read a File as a { media_type, data } attachment without re-encoding. */
	function readAsAttachment(file: File): Promise<ChatAttachmentPayload> {
		return new Promise<ChatAttachmentPayload>((resolve, reject) => {
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
	async function tryCompressImage(file: File): Promise<ChatAttachmentPayload | null> {
		if (typeof createImageBitmap !== 'function' || typeof document === 'undefined') return null;
		try {
			const bitmap = await createImageBitmap(file);
			let { width, height } = bitmap;
			const maxDim = Math.max(width, height);
			if (maxDim > maxImageDim) {
				const scale = maxImageDim / maxDim;
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
			const dataUrl = canvas.toDataURL('image/jpeg', jpegQuality);
			const comma = dataUrl.indexOf(',');
			return {
				media_type: 'image/jpeg',
				data: comma >= 0 ? dataUrl.slice(comma + 1) : dataUrl,
			};
		} catch {
			logger.warn('Composer', 'image compression failed, using original');
			return null;
		}
	}

	/**
	 * Convert a File to a { media_type, data } attachment (base64, no prefix).
	 * Compresses to JPEG when the result is smaller than the original;
	 * otherwise keeps the original encoding.
	 */
	async function fileToAttachment(file: File): Promise<ChatAttachmentPayload> {
		if (file.size > maxImageBytes) {
			throw new Error(`图片超过 ${Math.round(maxImageBytes / 1024 / 1024)}MB 上限`);
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
	function isImageFile(file: File): boolean {
		if (file.type && file.type.startsWith('image/')) return true;
		const ext = (file.name.split('.').pop() || '').toLowerCase();
		return IMAGE_EXTENSIONS.has(ext);
	}

	async function addPendingImages(files: FileList | File[]) {
		if (!files || files.length === 0) return;
		const room = maxImages - pendingImages.length - pendingImageReads;
		if (room <= 0) {
			addNotification(`最多支持 ${maxImages} 张图片`, 'error', 3000);
			return;
		}
		const list: File[] = Array.from(files).slice(0, room);
		const imageFiles = list.filter(isImageFile);
		for (const f of list) {
			if (!isImageFile(f)) {
				addNotification(`不支持的文件类型: ${f.name}`, 'error', 3000);
			}
		}
		pendingImageReads += imageFiles.length;
		pendingAttachmentReads += imageFiles.length;
		for (const f of imageFiles) {
			try {
				pendingImages = [
					...pendingImages,
					{ ...(await fileToAttachment(f)), previewKey: newAttachmentPreviewKey() },
				];
			} catch (e) {
				reportError(e, { context: 'Composer', message: '图片读取失败' });
			} finally {
				pendingImageReads -= 1;
				pendingAttachmentReads -= 1;
			}
		}
	}

	function handlePaste(e: ClipboardEvent) {
		const items = e.clipboardData?.items;
		if (!items) return;
		const images: File[] = [];
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

	function removePendingImage(previewKey: string) {
		pendingImages = pendingImages.filter((image) => image.previewKey !== previewKey);
	}

	function formatFileSize(bytes: number): string {
		if (bytes < 1024) return `${bytes} B`;
		if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
		return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
	}

	// Read non-image files as base64 attachments (with the original name) so
	// the backend can persist them to disk and hand the agent a path. Files
	// are capped at maxFiles / maxFileBytes, mirroring server validation.
	async function addPendingFiles(files: FileList | File[]) {
		if (!files || files.length === 0) return;
		const room = maxFiles - pendingFiles.length - pendingFileReads;
		if (room <= 0) {
			addNotification(`最多支持 ${maxFiles} 个文件`, 'error', 3000);
			return;
		}
		const list: File[] = Array.from(files).slice(0, room);
		const acceptedFiles = list.filter((f) => f.size <= maxFileBytes);
		for (const f of list) {
			if (f.size > maxFileBytes) {
				addNotification(
					`文件超过 ${Math.round(maxFileBytes / 1024 / 1024)}MB 上限: ${f.name}`,
					'error',
					3000,
				);
			}
		}
		pendingFileReads += acceptedFiles.length;
		pendingAttachmentReads += acceptedFiles.length;
		for (const f of acceptedFiles) {
			try {
				const { media_type, data } = await readAsAttachment(f);
				pendingFiles = [
					...pendingFiles,
					{
						media_type,
						data,
						filename: f.name,
						size: f.size,
						previewKey: newAttachmentPreviewKey(),
					},
				];
			} catch (e) {
				reportError(e, { context: 'Composer', message: '文件读取失败' });
			} finally {
				pendingFileReads -= 1;
				pendingAttachmentReads -= 1;
			}
		}
	}

	// Single entry point for the attachment picker: images (by MIME/extension)
	// go to the vision preview row, everything else to the file/audio row.
	function handleAttachSelect(e: Event) {
		const input = e.currentTarget;
		if (!(input instanceof HTMLInputElement)) return;
		const files: File[] = Array.from(input.files ?? []);
		const images = files.filter(isImageFile);
		const others = files.filter((f) => !isImageFile(f));
		if (images.length > 0) addPendingImages(images);
		if (others.length > 0) addPendingFiles(others);
		input.value = '';
	}

	function removePendingFile(previewKey: string) {
		pendingFiles = pendingFiles.filter((file) => file.previewKey !== previewKey);
	}

	// Collect whatever is currently pending (text, images, files) into a
	// single normalized payload and forward it to the host, then clear the
	// draft. The host owns the actual submission side effects.
	function handleSubmit() {
		if (pendingAttachmentReads > 0) {
			addNotification('附件仍在读取，请完成后再发送', 'info');
			return;
		}
		const text = transcriptInput.trim();
		const images = pendingImages.map(({ media_type, data }) => ({ media_type, data }));
		const files = pendingFiles.map(({ media_type, data, filename }) => ({
			media_type,
			data,
			filename,
		}));
		if (!text && images.length === 0 && files.length === 0 && !allowEmptyMessageSubmit) return;
		const submittedDeliveryMode = askAwaiting
			? askDeliveryMode === 'follow_up'
				? 'follow_up'
				: undefined
			: confirmationAwaiting || sessionStatus === 'paused'
				? 'follow_up'
				: deliveryModeAvailable
					? deliveryMode
					: undefined;
		cachedDrafts.delete(transcriptDraftSessionId);
		transcriptInput = '';
		pendingImages = [];
		pendingFiles = [];
		onsubmit?.({
			text,
			images,
			files,
			...(submittedDeliveryMode ? { deliveryMode: submittedDeliveryMode } : {}),
		});
	}

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
			e.preventDefault();
			handleSubmit();
		}
	}

	// Auto-grow the input to fit its content. While the content is a single
	// line, the vertical padding is balanced so the text renders centered
	// (matching the placeholder); multi-line content uses a fixed padding.
	const CHAT_INPUT_MIN_H = 48;
	const CHAT_INPUT_BASE_PAD = 10;
	function autoGrowInput() {
		const el = transcriptTextarea;
		if (!el) return;
		el.style.height = 'auto';
		el.style.paddingTop = CHAT_INPUT_BASE_PAD + 'px';
		el.style.paddingBottom = CHAT_INPUT_BASE_PAD + 'px';
		const contentH = el.scrollHeight;
		const singleLine = !transcriptInput.includes('\n') && contentH <= CHAT_INPUT_MIN_H;
		el.style.height = Math.max(CHAT_INPUT_MIN_H, contentH) + 'px';
		if (singleLine) {
			// Use the computed line height instead of a hard-coded font metric. The
			// input font falls back to a CJK system font on Windows, so its actual
			// line box can differ from the Latin token by a fraction of a pixel.
			const computed = getComputedStyle(el);
			const lineHeight = Number.parseFloat(computed.lineHeight);
			const borderHeight =
				Number.parseFloat(computed.borderTopWidth) +
				Number.parseFloat(computed.borderBottomWidth);
			const innerH = Math.max(0, el.clientHeight - borderHeight);
			const totalPad = Math.max(0, innerH - (Number.isFinite(lineHeight) ? lineHeight : 0));
			const pad = totalPad / 2;
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
		if (!browser || !transcriptTextarea) return;
		autoGrowInput();

		// Recalculate when the composer width changes. A draft can switch between
		// one and multiple visual lines without its value changing, otherwise the
		// old vertical padding would make the text appear intermittently off-center.
		// Observe width only: autoGrowInput writes the textarea height, and reacting
		// to that same height change during ResizeObserver delivery can cause a
		// resize-loop error when the window is being resized.
		if (typeof ResizeObserver !== 'function') return;
		let lastWidth = transcriptTextarea.clientWidth;
		let pendingResizeFrame: number | undefined;
		const observer = new ResizeObserver((entries) => {
			const width = entries[0]?.contentRect.width;
			if (width === undefined || width === lastWidth) return;
			lastWidth = width;
			if (pendingResizeFrame !== undefined) cancelAnimationFrame(pendingResizeFrame);
			pendingResizeFrame = requestAnimationFrame(() => {
				pendingResizeFrame = undefined;
				autoGrowInput();
			});
		});
		observer.observe(transcriptTextarea);
		return () => {
			observer.disconnect();
			if (pendingResizeFrame !== undefined) cancelAnimationFrame(pendingResizeFrame);
		};
	});

	function selectedRange() {
		const el = transcriptTextarea;
		if (!el) return { start: 0, end: 0, text: '' };
		const start = el.selectionStart ?? 0;
		const end = el.selectionEnd ?? 0;
		return { start, end, text: transcriptInput.slice(start, end) };
	}

	function setDraftAndCaret(next: string, caret: number) {
		transcriptInput = next;
		tick().then(() => {
			const el = transcriptTextarea;
			if (!el) return;
			el.focus();
			el.setSelectionRange(caret, caret);
		});
	}

	function handleContextMenu(e: MouseEvent) {
		openContextMenu(e, buildContextMenuItems(selectedRange()));
	}

	async function handleCtxCopy({ text: selected }: Pick<TextSelection, 'text'>) {
		await copyText(selected || transcriptInput, selected ? '选中' : '输入');
	}

	async function handleCtxCut({ start, end, text: selText }: TextSelection) {
		if (!selText) return;
		const ok = await copyText(selText, '选中');
		if (!ok) return;
		setDraftAndCaret(transcriptInput.slice(0, start) + transcriptInput.slice(end), start);
	}

	async function handleCtxPaste({ start, end }: Pick<TextSelection, 'start' | 'end'>) {
		try {
			const text = await navigator.clipboard.readText();
			setDraftAndCaret(
				transcriptInput.slice(0, start) + (text ?? '') + transcriptInput.slice(end),
				start + (text ?? '').length,
			);
		} catch (error) {
			reportError(error, { context: 'Composer', message: '粘贴失败' });
		}
	}

	function handleCtxSelectAll() {
		const el = transcriptTextarea;
		if (!el) return;
		el.focus();
		el.setSelectionRange(0, transcriptInput.length);
	}

	function handleCtxClear() {
		transcriptInput = '';
		tick().then(() => transcriptTextarea?.focus());
	}

	function buildContextMenuItems(range: TextSelection): ContextMenuItem[] {
		const hasSel = range.text.length > 0;
		const hasText = transcriptInput.length > 0;
		return [
			{
				id: 'cut',
				label: '剪切',
				icon: 'cut',
				disabled: !hasSel,
				action: () => handleCtxCut(range),
			},
			{
				id: 'copy',
				label: hasSel ? '复制选中' : '复制',
				icon: 'copy',
				disabled: !hasText,
				action: () => handleCtxCopy(range),
			},
			{ id: 'paste', label: '粘贴', icon: 'paste', action: () => handleCtxPaste(range) },
			{ id: 'sep', separator: true },
			{
				id: 'selectAll',
				label: '全选',
				icon: 'selectAll',
				disabled: !hasText,
				action: handleCtxSelectAll,
			},
			{
				id: 'clear',
				label: '清空',
				icon: 'delete',
				danger: true,
				disabled: !hasText,
				action: handleCtxClear,
			},
		];
	}
</script>

<div class="input-area">
	{#if pendingFiles.length > 0}
		<div class="file-preview-row">
			{#each pendingFiles as file (file.previewKey)}
				<div class="file-preview" in:fly={{ y: 4, duration: 180, easing: cubicOut }}>
					{#if (file.media_type || '').startsWith('video/') && file.data}
						<video
							class="file-preview-video"
							controls
							preload="metadata"
							src={mediaDataUrl(file)}
						>
							<track kind="captions" />
						</video>
					{:else if (file.media_type || '').startsWith('audio/') && file.data}
						<audio
							class="file-preview-audio"
							controls
							preload="none"
							src={mediaDataUrl(file)}
							title={file.filename || '音频附件'}
						></audio>
						<div class="file-preview-info">
							<span class="file-preview-name">{file.filename}</span>
							<span class="file-preview-size">音频 · {formatFileSize(file.size)}</span
							>
						</div>
					{:else}
						<Icon name="file" size={18} className="file-preview-icon" />
						<div class="file-preview-info">
							<span class="file-preview-name">{file.filename}</span>
							<span class="file-preview-size">{formatFileSize(file.size)}</span>
						</div>
					{/if}
					<MaterialIconButton
						icon="close"
						className="file-preview-remove"
						label={`移除附件 ${file.filename}`}
						title={`移除附件 ${file.filename}`}
						onclick={() => removePendingFile(file.previewKey)}
					/>
				</div>
			{/each}
		</div>
	{/if}
	{#if pendingImages.length > 0}
		<div class="image-preview-row">
			{#each pendingImages as img, index (img.previewKey)}
				<div class="image-preview" in:fly={{ y: 4, duration: 180, easing: cubicOut }}>
					<img src={mediaDataUrl(img)} alt="待发送图片" />
					<MaterialIconButton
						icon="close"
						className="image-preview-remove"
						label={`移除第 ${index + 1} 张待发送图片`}
						onclick={() => removePendingImage(img.previewKey)}
					/>
				</div>
			{/each}
		</div>
	{/if}
	<div class="input-row">
		<label class="sr-only" for="chat-input">消息输入框</label>
		<textarea
			bind:this={transcriptTextarea}
			id="chat-input"
			rows="1"
			placeholder={chatInputPlaceholder}
			bind:value={transcriptInput}
			onkeydown={handleKeydown}
			onpaste={handlePaste}
			oncontextmenu={handleContextMenu}
			class="md-input chat-input"
			autocomplete="off"></textarea>
	</div>
	<div class="toolbar-row md-toolbar">
		<div class="toolbar-left">
			{@render toolbarLeft?.()}
		</div>
		<div class="toolbar-right">
			{#if deliveryModeAvailable && (!confirmationAwaiting || askAwaiting)}
				<MaterialSelect
					id="chat-delivery-mode"
					ariaLabel={askAwaiting ? '问题回复方式' : '消息投递方式'}
					width="compact"
					value={selectedDeliveryMode}
					options={deliveryModeOptions}
					disabled={interrupting}
					title={askAwaiting
						? askDeliveryMode === 'follow_up'
							? '排队等待当前工作结束；不会回答或唤醒 Ask'
							: '将输入和已选选项作为当前问题的答案'
						: deliveryMode === 'follow_up'
							? '等待当前工作自然完成后追加'
							: '在下一个安全边界调整当前工作，不中断当前请求或工具'}
					onChange={(value) => {
						if (askAwaiting) {
							if (value === 'auto' || value === 'follow_up') askDeliveryMode = value;
						} else if (value === 'steering' || value === 'follow_up') {
							deliveryMode = value;
						}
					}}
				/>
			{/if}
			<MaterialIconButton
				size="toolbar"
				label="添加附件"
				title="添加图片、音频或文件"
				icon="paperclip"
				onclick={() => attachFileInput?.click()}
			></MaterialIconButton>
			<input
				hidden
				type="file"
				multiple
				bind:this={attachFileInput}
				onchange={handleAttachSelect}
			/>
			<MaterialIconButton
				size="toolbar"
				variant={recordingState.isRecording ? 'danger' : 'default'}
				label={recordingState.isRecording ? '停止录音' : '开始录音'}
				title={recordingState.isRecording ? '停止录音' : '开始录音'}
				icon={recordingState.isRecording ? 'pause' : 'mic'}
				onclick={handleRecordClick}
			></MaterialIconButton>
			{@render toolbarRight?.()}
			<MaterialIconButton
				size="toolbar"
				variant={stopMode ? 'danger' : 'primary'}
				label={hasInput ? '发送' : stopMode ? '中断输出' : '发送'}
				title={pendingAttachmentReads > 0
					? '附件正在读取'
					: hasInput
						? '发送'
						: stopMode
							? '中断当前输出'
							: '发送'}
				ariaBusy={interrupting || pendingAttachmentReads > 0}
				disabled={interrupting ||
					pendingAttachmentReads > 0 ||
					(!hasInput && !isGenerating && !sessionRunning)}
				icon={hasInput ? 'send' : stopMode ? 'stop' : 'send'}
				onclick={stopMode ? () => onstop?.() : handleSubmit}
			></MaterialIconButton>
		</div>
	</div>
</div>

<style>
	.input-area {
		position: absolute;
		left: 50%;
		bottom: var(--md-sys-space-md);
		transform: translateX(-50%);
		z-index: 4;
		background: transparent;
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-large);
		box-shadow: var(--md-sys-elevation-1);
		padding: var(--md-sys-space-md) var(--md-sys-space-lg) var(--md-sys-space-xs);
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
		flex-shrink: 0;
		/* Match the message column's horizontal inset so the composer edge,
		 * message bubbles, and the jump-to-bottom control share one guide. */
		max-width: min(
			calc(
				var(--md-sys-chat-max-width) - 2 * var(--chat-inline-gutter, var(--md-sys-space-md))
			),
			calc(100% - 2 * var(--chat-inline-gutter, var(--md-sys-space-md)))
		);
		margin: 0;
		width: 100%;
		transition:
			border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			box-shadow var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.input-area::before {
		content: '';
		position: absolute;
		inset: 1px;
		border-radius: calc(var(--md-sys-shape-large) - 1px);
		background: var(--md-sys-color-surface-container-low);
		pointer-events: none;
		z-index: 0;
	}
	.input-area > * {
		position: relative;
		z-index: 1;
	}
	.input-area:has(.chat-input:focus) {
		border-color: var(--md-sys-color-primary);
		box-shadow: var(--md-sys-focus-ring);
	}

	.image-preview-row {
		display: flex;
		overflow-x: auto;
		max-height: 96px;
		gap: var(--md-sys-space-sm);
	}
	.image-preview {
		flex-shrink: 0;
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
	:global(.md-icon-btn.image-preview-remove) {
		position: absolute;
		top: var(--md-sys-space-2xs);
		right: var(--md-sys-space-2xs);
		width: var(--md-comp-icon-button-compact-size);
		height: var(--md-comp-icon-button-compact-size);
		min-width: var(--md-comp-icon-button-compact-size);
		min-height: var(--md-comp-icon-button-compact-size);
		border-color: transparent;
		background: color-mix(in srgb, var(--md-sys-color-scrim) 60%, transparent);
		color: var(--md-sys-color-on-surface);
	}

	.file-preview-row {
		display: flex;
		overflow-x: auto;
		max-height: min(160px, 20dvh);
		gap: var(--md-sys-space-sm);
	}
	.file-preview {
		flex-shrink: 0;
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		max-width: 260px;
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		border: 1px solid var(--md-sys-color-outline-variant);
		background: var(--md-sys-color-surface-container-high);
	}
	:global(.file-preview-icon) {
		flex-shrink: 0;
		color: var(--md-sys-color-on-surface-variant);
	}
	.file-preview-info {
		min-width: 0;
		display: flex;
		flex-direction: column;
	}
	.file-preview-audio {
		width: 150px;
		height: 30px;
		flex: 0 0 auto;
	}
	.file-preview-video {
		width: min(100%, 240px);
		max-height: 160px;
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-surface-container-high);
	}
	.file-preview-name {
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		max-width: 170px;
	}
	.file-preview-size {
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
	}
	:global(.md-icon-btn.file-preview-remove) {
		width: var(--md-comp-icon-button-compact-size);
		height: var(--md-comp-icon-button-compact-size);
		min-width: var(--md-comp-icon-button-compact-size);
		min-height: var(--md-comp-icon-button-compact-size);
		margin-left: auto;
		border-color: transparent;
		flex-shrink: 0;
		background: var(--md-sys-color-surface-container-highest);
		color: var(--md-sys-color-on-surface-variant);
	}
	:global(.md-icon-btn.file-preview-remove:hover) {
		background: var(--md-sys-color-error-container);
		color: var(--md-sys-color-on-error-container);
	}
	.input-row {
		display: flex;
		gap: 0;
		align-items: flex-end;
		min-width: 0;
	}
	.chat-input {
		max-height: min(240px, 28dvh);
		--chat-pad: 10px;
		background: transparent;
		border: none;
		border-radius: var(--md-sys-shape-small);
		min-height: 48px;
		height: auto;
		flex: 1;
		min-width: 0;
		padding: var(--chat-pad) var(--md-sys-space-sm);
		resize: none;
		overflow-y: auto;
		line-height: var(--md-sys-typescale-body-medium-line-height);
		font-size: var(--md-sys-typescale-body-medium-size);
		text-align: left;
	}
	.chat-input::placeholder {
		/* Placeholder line-height tracks the balanced padding so it stays
		   vertically centered exactly like the (balanced) input text. */
		line-height: var(--md-sys-typescale-body-medium-line-height);
	}
	.chat-input:focus {
		border: none;
		padding: var(--chat-pad) var(--md-sys-space-sm);
		box-shadow: none;
	}
	.chat-input:focus-visible {
		box-shadow: none;
	}

	.toolbar-row {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: var(--md-comp-toolbar-gap);
		padding-inline: 0;
		padding-top: var(--md-sys-space-xs);
		border-top: 1px solid var(--md-sys-color-outline-variant);
	}
	.toolbar-left {
		flex: 0 0 auto;
		min-width: 0;
		flex-wrap: wrap;
		display: flex;
		align-items: center;
		gap: var(--md-comp-toolbar-gap);
	}
	.toolbar-right {
		flex: 0 0 auto;
		min-width: 0;
		display: flex;
		align-items: center;
		gap: var(--md-comp-toolbar-gap);
		margin-left: auto;
		flex-wrap: wrap;
		justify-content: flex-end;
		max-width: 100%;
	}
	.toolbar-row :global(.md-btn) {
		height: var(--md-comp-toolbar-height);
		padding: 0 var(--md-sys-space-md);
		font-size: var(--md-sys-typescale-label-large-size);
		line-height: var(--md-sys-typescale-label-large-line-height);
	}

	@container conversation (max-width: 700px) {
		.input-area {
			padding-inline: var(--md-sys-space-md);
		}
		.toolbar-row {
			align-items: flex-start;
		}
		.toolbar-left,
		.toolbar-right {
			flex: 1 1 auto;
		}
		.toolbar-right {
			justify-content: flex-end;
			margin-left: 0;
		}
	}

	@container conversation (max-width: 455px) {
		.toolbar-left,
		.toolbar-right {
			width: 100%;
		}
		.toolbar-right {
			/* Keep the four action buttons as one compact cluster on narrow
			 * windows instead of stretching them across the whole row. */
			justify-content: flex-end;
		}
	}

	@container conversation (min-width: 760px) {
		.input-area {
			--chat-inline-gutter: var(--md-sys-space-2xl);
		}
	}

	@media screen and (max-height: 560px) {
		.input-area {
			bottom: var(--md-sys-space-xs);
			gap: var(--md-sys-space-2xs);
			padding: var(--md-sys-space-xs) var(--md-sys-space-sm) 2px;
			border-radius: var(--md-sys-shape-medium);
		}
		.input-area::before {
			border-radius: calc(var(--md-sys-shape-medium) - 1px);
		}
		.chat-input {
			min-height: 40px;
			max-height: 88px;
			padding-block: var(--md-sys-space-sm);
		}
		.toolbar-row {
			flex-wrap: wrap;
			align-items: center;
			gap: var(--md-sys-space-xs);
			padding-top: 0;
			border-top: 0;
		}
		.toolbar-left {
			flex: 0 1 auto;
			flex-wrap: wrap;
		}
		.toolbar-right {
			flex: 0 0 auto;
			gap: var(--md-sys-space-xs);
			margin-left: auto;
		}
		:global(.toolbar-right .md-select-container) {
			max-width: 120px;
		}
		:global(.model-switch) {
			max-width: 130px;
		}
	}
</style>
