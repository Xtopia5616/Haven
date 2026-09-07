<script>
	import { tick } from 'svelte';
	import logger from '$lib/logger.ts';
	import { browser } from '$app/environment';
	import { invoke } from '$lib/tauri.ts';
	import { addNotification, recordingOverlay, imageDataUrl } from '$lib/stores.ts';
	import { reportError } from '$lib/errorHandling.ts';
	import { formatError } from '$lib/formatError.ts';
	import { syncStore } from '$lib/syncStore.ts';
	import ContextMenu from '$lib/ContextMenu.svelte';
	import MaterialIconButton from '$lib/MaterialIconButton.svelte';
	import { copyText } from '$lib/clipboard.ts';

	let {
		activeSessionId = null,
		hotkeyBinding = 'Ctrl+Shift+Space',
		isGenerating = false,
		sessionRunning = false,
		interrupting = false,
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
	} = $props();

	// Pending image attachments (multimodal): [{ mediaType, data }] with data
	// holding base64 bytes (no data: prefix). Filled by paste / file picker,
	// sent along with the next message, cleared on submit.
	/** @type {any[]} */
	let pendingImages = $state([]);

	// Pending non-image file attachments: [{ media_type, data, filename, size }].
	// Read as base64 when picked, persisted by the backend to disk and handed
	// to the agent as a path the files tool can read.
	/** @type {any[]} */
	let pendingFiles = $state([]);
	// Single hidden picker for both images and files; the picked items are
	// split by type on selection (images -> pendingImages, rest -> pendingFiles).
	let attachFileInput = /** @type {HTMLInputElement | null} */ ($state(null));

	// Recording state (mirror of the global recordingOverlay store) so the
	// toolbar mic button can toggle start/stop inline.
	let recordingState = $state({ isRecording: false });
	$effect(() =>
		syncStore(recordingOverlay, (v) => {
			recordingState = v;
		}),
	);

	let transcriptInput = $state('');
	let transcriptTextarea = /** @type {HTMLTextAreaElement | null} */ ($state(null));

	const hasDraft = $derived(
		transcriptInput.trim().length > 0 || pendingImages.length > 0 || pendingFiles.length > 0,
	);
	// Treat selected ask chips (allowEmptySubmit) as submit-ready input so the
	// send button stays enabled and does not flip into stop mode.
	const hasInput = $derived(hasDraft || allowEmptySubmit);
	// While the agent is generating, a sent message is delivered immediately
	// to the backend: the agent injects it in the gap between tool calls and
	// the final content, so it can steer the answer instead of waiting for
	// the whole turn to finish.
	// The merged send button becomes "interrupt output" only when there is no input
	// and the agent is actively working (generating output, a running/pending
	// session). With fresh input present, it always stays a send button.
	const stopMode = $derived(!hasInput && (interrupting || isGenerating || sessionRunning));

	// Allow the host page to populate the draft box programmatically (e.g.
	// restoring a message after rollback) via `bind:this`.
	/** @param {string} text */
	export function setDraft(text) {
		transcriptInput = text ?? '';
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
			reportError(e, { context: 'InputRouter', message: '录音失败' });
		}
	}

	/** Read a File as a { media_type, data } attachment without re-encoding. */
	/** @param {File} file */
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
	/** @param {File} file */
	async function tryCompressImage(file) {
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
		} catch (e) {
			logger.warn('InputRouter', 'image compression failed, using original', e);
			return null;
		}
	}

	/**
	 * Convert a File to a { media_type, data } attachment (base64, no prefix).
	 * Compresses to JPEG when the result is smaller than the original;
	 * otherwise keeps the original encoding.
	 */
	/** @param {File} file */
	async function fileToAttachment(file) {
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
	/** @param {File} file */
	function isImageFile(file) {
		if (file.type && file.type.startsWith('image/')) return true;
		const ext = (file.name.split('.').pop() || '').toLowerCase();
		return IMAGE_EXTENSIONS.has(ext);
	}

	/** @param {FileList | File[]} files */
	async function addPendingImages(files) {
		if (!files || files.length === 0) return;
		const room = maxImages - pendingImages.length;
		if (room <= 0) {
			addNotification(`最多支持 ${maxImages} 张图片`, 'error', 3000);
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
				reportError(e, { context: 'InputRouter', message: '图片读取失败' });
			}
		}
	}

	/** @param {ClipboardEvent} e */
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

	/** @param {number} index */
	function removePendingImage(index) {
		pendingImages = pendingImages.filter((_, i) => i !== index);
	}

	/** @param {number} bytes */
	function formatFileSize(bytes) {
		if (bytes < 1024) return `${bytes} B`;
		if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
		return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
	}

	// Read non-image files as base64 attachments (with the original name) so
	// the backend can persist them to disk and hand the agent a path. Files
	// are capped at maxFiles / maxFileBytes, mirroring server validation.
	/** @param {FileList | File[]} files */
	async function addPendingFiles(files) {
		if (!files || files.length === 0) return;
		const room = maxFiles - pendingFiles.length;
		if (room <= 0) {
			addNotification(`最多支持 ${maxFiles} 个文件`, 'error', 3000);
			return;
		}
		const list = Array.from(files).slice(0, room);
		for (const f of list) {
			if (f.size > maxFileBytes) {
				addNotification(
					`文件超过 ${Math.round(maxFileBytes / 1024 / 1024)}MB 上限: ${f.name}`,
					'error',
					3000,
				);
				continue;
			}
			try {
				const { media_type, data } = await readAsAttachment(f);
				pendingFiles = [
					...pendingFiles,
					{ media_type, data, filename: f.name, size: f.size },
				];
			} catch (e) {
				reportError(e, { context: 'InputRouter', message: '文件读取失败' });
			}
		}
	}

	// Single entry point for the attachment picker: images (by MIME/extension)
	// go to the vision preview row, everything else to the file chips.
	/** @param {any} e */
	function handleAttachSelect(e) {
		const files = Array.from(e.target.files || []);
		const images = files.filter(isImageFile);
		const others = files.filter((f) => !isImageFile(f));
		if (images.length > 0) addPendingImages(images);
		if (others.length > 0) addPendingFiles(others);
		e.target.value = '';
	}

	/** @param {number} index */
	function removePendingFile(index) {
		pendingFiles = pendingFiles.filter((_, i) => i !== index);
	}

	// Collect whatever is currently pending (text, images, files) into a
	// single normalized payload and forward it to the host, then clear the
	// draft. The host owns the actual submission side effects.
	function handleSubmit() {
		const text = transcriptInput.trim();
		const images = pendingImages;
		const files = pendingFiles;
		if (!text && images.length === 0 && files.length === 0 && !allowEmptySubmit) return;
		transcriptInput = '';
		pendingImages = [];
		pendingFiles = [];
		onsubmit?.({ text, images, files });
	}

	/** @param {KeyboardEvent} e */
	function handleKeydown(e) {
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
		if (browser) autoGrowInput();
	});

	let ctxMenu = $state({ open: false, x: 0, y: 0, selStart: 0, selEnd: 0, selText: '' });

	function closeCtxMenu() {
		ctxMenu = { open: false, x: 0, y: 0, selStart: 0, selEnd: 0, selText: '' };
	}

	function selectedRange() {
		const el = transcriptTextarea;
		if (!el) return { start: 0, end: 0, text: '' };
		const start = el.selectionStart ?? 0;
		const end = el.selectionEnd ?? 0;
		return { start, end, text: transcriptInput.slice(start, end) };
	}

	/** @param {string} next @param {number} caret */
	function setDraftAndCaret(next, caret) {
		transcriptInput = next;
		tick().then(() => {
			const el = transcriptTextarea;
			if (!el) return;
			el.focus();
			el.setSelectionRange(caret, caret);
		});
	}

	/** @param {MouseEvent} e */
	function handleContextMenu(e) {
		e.preventDefault();
		e.stopPropagation();
		const { start, end, text } = selectedRange();
		ctxMenu = {
			open: true,
			x: e.clientX,
			y: e.clientY,
			selStart: start,
			selEnd: end,
			selText: text,
		};
	}

	async function handleCtxCopy() {
		const selected = ctxMenu.selText;
		await copyText(selected || transcriptInput, selected ? '选中' : '输入');
	}

	async function handleCtxCut() {
		const { selText, selStart, selEnd } = ctxMenu;
		if (!selText) return;
		const ok = await copyText(selText, '选中');
		if (!ok) return;
		setDraftAndCaret(
			transcriptInput.slice(0, selStart) + transcriptInput.slice(selEnd),
			selStart,
		);
	}

	async function handleCtxPaste() {
		const start = ctxMenu.selStart;
		const end = ctxMenu.selEnd;
		try {
			const text = await navigator.clipboard.readText();
			setDraftAndCaret(
				transcriptInput.slice(0, start) + (text ?? '') + transcriptInput.slice(end),
				start + (text ?? '').length,
			);
		} catch (error) {
			logger.warn('InputRouter', 'clipboard paste failed', formatError(error));
			addNotification('粘贴失败', 'error', 2000);
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

	let ctxMenuItems = $derived.by(() => {
		const hasSel = ctxMenu.selText.length > 0;
		const hasText = transcriptInput.length > 0;
		return [
			{ id: 'cut', label: '剪切', icon: 'cut', disabled: !hasSel, action: handleCtxCut },
			{
				id: 'copy',
				label: hasSel ? '复制选中' : '复制',
				icon: 'copy',
				disabled: !hasText,
				action: handleCtxCopy,
			},
			{ id: 'paste', label: '粘贴', icon: 'paste', action: handleCtxPaste },
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
	});
</script>

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
		<label class="sr-only" for="chat-input">消息输入框</label>
		<textarea
			bind:this={transcriptTextarea}
			id="chat-input"
			rows="1"
			placeholder={activeSessionId
				? `追加指令，Enter 发送，Shift+Enter 换行；按 ${hotkeyBinding} 录音`
				: `输入指令，Enter 发送，或按 ${hotkeyBinding} 录音`}
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
			<MaterialIconButton
				size="toolbar"
				label="添加附件"
				title="添加图片或文件"
				onclick={() => attachFileInput?.click()}
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
			</MaterialIconButton>
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
				onclick={handleRecordClick}
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
			</MaterialIconButton>
			{@render toolbarRight?.()}
			<MaterialIconButton
				size="toolbar"
				variant={stopMode ? 'danger' : 'primary'}
				label={hasInput ? '发送' : stopMode ? '中断输出' : '发送'}
				title={hasInput ? '发送' : stopMode ? '中断当前输出' : '发送'}
				ariaBusy={interrupting}
				disabled={interrupting || (!hasInput && !isGenerating && !sessionRunning)}
				onclick={stopMode ? () => onstop?.() : handleSubmit}
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
			</MaterialIconButton>
		</div>
	</div>
</div>

<ContextMenu
	open={ctxMenu.open}
	x={ctxMenu.x}
	y={ctxMenu.y}
	items={ctxMenuItems}
	onClose={closeCtxMenu}
/>

<style>
	.input-area {
		background: var(--md-sys-color-surface-container-low);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-large);
		box-shadow: var(--md-sys-elevation-1);
		padding: var(--md-sys-space-md) var(--md-sys-space-lg) var(--md-sys-space-xs);
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
		flex-shrink: 0;
		max-width: min(800px, calc(100% - 2 * var(--md-sys-content-gutter)));
		margin: 0 auto var(--md-sys-space-md);
		width: 100%;
		transition:
			border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			box-shadow var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.input-area:has(.chat-input:focus) {
		border-color: var(--md-sys-color-primary);
		box-shadow: var(--md-sys-focus-ring);
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
	.input-row {
		display: flex;
		gap: 0;
		align-items: flex-end;
		min-width: 0;
	}
	.sr-only {
		position: absolute;
		width: 1px;
		height: 1px;
		padding: 0;
		margin: -1px;
		overflow: hidden;
		clip: rect(0, 0, 0, 0);
		white-space: nowrap;
		border: 0;
	}
	.chat-input {
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
	}
	.toolbar-row :global(.md-btn) {
		height: var(--md-comp-toolbar-height);
		padding: 0 var(--md-sys-space-md);
		font-size: var(--md-sys-typescale-label-large-size);
		line-height: var(--md-sys-typescale-label-large-line-height);
	}

	@media (max-width: 700px) {
		.input-area {
			max-width: calc(100% - 2 * var(--md-sys-content-gutter));
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

	@media (max-width: 455px) {
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
</style>
