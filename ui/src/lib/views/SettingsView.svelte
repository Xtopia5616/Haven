<script>
	import { onMount, onDestroy, tick } from 'svelte';
	import { invoke } from '$lib/tauri.ts';
	import { registerListeners } from '$lib/events.ts';
	import { themeStore } from '$lib/themeStore.ts';
	import MaterialSwitch from '$lib/MaterialSwitch.svelte';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialNumberField from '$lib/MaterialNumberField.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import MaterialCollapsible from '$lib/MaterialCollapsible.svelte';
	import HotkeyInput from '$lib/HotkeyInput.svelte';
	import { addNotification } from '$lib/stores.ts';
	import { formatError } from '$lib/formatError.ts';
	import { registerSettingsLeaveGuard } from '$lib/settingsGuard.ts';
	import { ensureRoleSlots } from '$lib/modelRoles.ts';
	import {
		parseApiKeyStatus,
		parseLogInfo,
		parseLogTail,
		parseShellAvailability,
	} from '$lib/contracts/settings.ts';
	import ModelSettings from './ModelSettings.svelte';
	import logger from '$lib/logger.ts';

	let llmConfig = $state({
		// Named LLM providers (connection-level endpoints). Roles reference
		// these by name and pick a model id from the provider's fetched model
		// list. Each entry: { name, provider, api_style, base_url, api_key,
		// default_max_tokens?, default_temperature?, ... }.
		providers: [],
		// Role→(provider, model) assignments:
		// { role, provider, model, temperature?, context_window?,
		//   cost_per_1k_input_tokens?, cost_per_1k_output_tokens?,
		//   cost_per_1k_cache_read_tokens?, cost_per_1k_cache_write_tokens?,
		//   max_tokens?, reasoning_effort?, web_search? }
		roles: [],
		stt_use_audio_model: true,
		vision_use_image_model: true,
		max_concurrent_requests: 2,
	});

	/** @type {{ small_model: boolean; default_model: boolean; balanced_model: boolean; image_model: boolean; audio_model: boolean; embedding_model: boolean; stt: boolean; ocr: boolean; ocr_secret: boolean; [key: string]: boolean }} */
	let keyConfigured = $state({
		small_model: false,
		default_model: false,
		balanced_model: false,
		image_model: false,
		audio_model: false,
		embedding_model: false,
		stt: false,
		ocr: false,
		ocr_secret: false,
	});

	// Per-provider api_key configured status (from get_api_key_status.
	// `providers`).
	let keyConfiguredProviders = $state({});

	// Per-card model discovery (role cards, STT) and TTS / 文生图 UI live in ModelSettings.

	let hotkeyMode = $state('toggle');
	let hotkeyBinding = $state('Ctrl+Shift+Space');
	let autostartEnabled = $state(false);

	let audio = $state({
		sample_rate: 16000,
		channels: 1,
		bits_per_sample: 16,
		max_duration_secs: 60,
		silence_timeout_ms: 1500,
		vad_threshold: 0.5,
	});

	let session = $state({ max_concurrent: 3, max_steps: 30 });
	/** @type {Record<string, number>} */
	let contextLimits = $state({
		compaction_ratio: 0.75,
		compaction_reserve_tokens: 4096,
		default_context_window: 128000,
		max_response_tokens: 1000000,
		max_observation_chars: 8000,
		max_transcript_chars: 4000,
		max_attachment_images: 4,
		max_attachment_files: 5,
		max_attachment_image_bytes: 10 * 1024 * 1024,
		max_attachment_file_bytes: 20 * 1024 * 1024,
		max_attachment_image_dim_px: 1568,
		attachment_image_jpeg_quality: 0.85,
		file_read_max_chars: 128000,
		file_line_span: 100,
		file_max_line_chars: 128000,
		file_summary_input_chars: 60000,
		file_max_list_entries: 1000,
		file_max_byte_read: 16 * 1024 * 1024,
		file_vision_max_bytes: 8 * 1024 * 1024,
		search_snippet_chars: 200,
		search_max_results: 1000,
		search_max_file_size_bytes: 100 * 1024 * 1024,
		search_window_bytes: 16 * 1024 * 1024,
		notification_summary_chars: 800,
		partial_checkpoint_min_chars: 1000,
		partial_checkpoint_interval_secs: 2,
		fact_infer_interval_steps: 25,
		max_known_facts: 40,
		sanitize_field_max_chars: 256,
		file_summary_timeout_secs: 120,
		cut_off_retries: 2,
		empty_response_max_retries: 3,
		empty_response_retry_delay_ms: 1500,
		stream_stall_warn_delay_ms: 10000,
		reasoning_echo_max_chars: 3000,
		background_job_tail_max_chars: 2000,
		background_job_output_emit_interval_ms: 1500,
		terminal_job_ttl_secs: 600,
		mcp_max_binary_payload_bytes: 2 * 1024 * 1024,
		mcp_max_sse_buffer_bytes: 2 * 1024 * 1024,
		skills_max_md_bytes: 256 * 1024,
		skills_max_parse_lines: 5000,
		skills_max_line_len: 4096,
		self_tool_max_instructions_bytes: 256 * 1024,
		self_tool_max_script_bytes: 512 * 1024,
		network_max_retries: 2,
		network_backoff_base_secs: 1,
		network_max_body_bytes: 1024 * 1024,
		clipboard_history_entries: 10,
		clipboard_history_max_entries: 100,
		clipboard_entry_max_chars: 2000,
		scheduled_actions_max: 32,
		reminders_due_horizon_secs: 365 * 24 * 3600,
		background_max_actions: 64,
		event_chunk_batch_max_bytes: 8 * 1024,
		input_ring_buffer_secs: 20,
		embedding_chunk_size: 64,
		max_tools_per_request: 128,
	});

	// Data-driven limit editor. `danger: true` fields get a warning badge and
	// red styling: raising them widens the memory / cost / attack surface.
	const LIMIT_GROUPS = [
		{
			id: 'context',
			title: '上下文与压缩',
			hint: '模型上下文窗口与自动压缩（compaction）行为的阈值。压缩阈值过高可能导致上下文溢出。',
			fields: [
				{ key: 'default_context_window', label: '默认上下文窗口', unit: 'K', kTokens: true, step: 1, danger: true, hint: '角色未填写 Context 且 Provider /models 未返回上下文长度时的回退窗口（单位 K = 1000 tokens，如 128 = 128K）。调高会增大每次请求的成本与溢出风险。' },
				{ key: 'max_response_tokens', label: '回复输出 token 下限', unit: 'tokens', danger: false, hint: '每个模型端点的 max_tokens 会被抬到不低于此值（取两者较大）。默认极大，长回复不会被截断；需要限制输出长度时调低此项。' },
				{ key: 'compaction_ratio', label: '压缩触发比例', unit: '0–1', step: 0.01, min: 0.1, max: 0.95, danger: true, hint: '历史占用窗口的比例达到该值时开始压缩。调高 = 更晚压缩 = 更接近溢出。' },
				{ key: 'compaction_reserve_tokens', label: '压缩保留 token', unit: 'tokens', danger: false, hint: '计算压缩阈值时为模型回复预留的 token 数。' },
				{ key: 'max_observation_chars', label: '工具观察字符上限', unit: 'chars', danger: true, hint: '工具结果进入对话的最大字符数，也是 shell/file/process 等工具的默认输出截断上限（per-tool 可覆盖）。调大直接推高 token 成本。' },
				{ key: 'max_tools_per_request', label: '单次请求工具数上限', unit: 'count', danger: true, hint: '发给模型的 tools 数组最大长度。默认 128（提供方硬顶约 350）。内置工具优先保留；load_mcp 可按 tool_names 只加载子集，整服超限时返回工具目录供再选。' },
				{ key: 'max_transcript_chars', label: '记忆提取转录上限', unit: 'chars', danger: true, hint: '事实提取时发送给模型的转录长度。' },
				{ key: 'notification_summary_chars', label: '通知摘要字符上限', unit: 'chars', danger: false },
				{ key: 'partial_checkpoint_min_chars', label: '流式检查点最小增量', unit: 'chars', danger: false, hint: '部分回复累计新增多少字符后落盘一次（崩溃恢复粒度）。' },
				{ key: 'partial_checkpoint_interval_secs', label: '流式检查点间隔', unit: 'secs', danger: false },
				{ key: 'fact_infer_interval_steps', label: '事实推断间隔', unit: 'steps', danger: false, hint: '长会话每多少步重新做一次事实提取。调小增加调用成本。' },
				{ key: 'max_known_facts', label: '提示中已知事实数', unit: 'count', danger: false },
				{ key: 'sanitize_field_max_chars', label: '事实字段消毒长度', unit: 'chars', danger: true, hint: '事实字段注入到系统提示前的截断长度。调大 = 更大提示注入面。' },
			],
		},
		{
			id: 'files',
			title: '文件与搜索工具',
			hint: 'files 工具读取、总结与搜索的资源上限。',
			fields: [
				{ key: 'file_read_max_chars', label: '文件全读上限', unit: 'chars', danger: true, hint: '超过该大小的文件不整体读取，改用 offset/limit 分段。调大 = 大文件整读内存风险。' },
				{ key: 'file_max_byte_read', label: '字节读取绝对上限', unit: 'bytes', mb: true, danger: true, hint: 'byte 模式单次读取的安全上限（不受调用方 limit 影响）。' },
				{ key: 'file_line_span', label: '行模式默认跨度', unit: 'lines', danger: false },
				{ key: 'file_max_line_chars', label: '单行缓冲上限', unit: 'chars', danger: true, hint: '病态单行文件（压缩包/超长行）的缓冲上限。' },
				{ key: 'file_summary_input_chars', label: '总结输入预算', unit: 'chars', danger: true, hint: '发送给 small_model 的总结输入上限。调大 = 更多 token。' },
				{ key: 'file_summary_timeout_secs', label: '总结超时', unit: 'secs', danger: false },
				{ key: 'file_max_list_entries', label: '目录列表条目上限', unit: 'count', danger: true },
				{ key: 'file_vision_max_bytes', label: '图片理解大小上限', unit: 'MB', mb: true, danger: true, hint: '超过该大小的图片拒绝送视觉模型。' },
				{ key: 'search_snippet_chars', label: '搜索片段长度', unit: 'chars', danger: false },
				{ key: 'search_max_results', label: '搜索结果上限', unit: 'count', danger: true },
				{ key: 'search_max_file_size_bytes', label: '搜索跳过文件大小', unit: 'MB', mb: true, danger: true },
				{ key: 'search_window_bytes', label: '行范围搜索窗口', unit: 'MB', mb: true, danger: true },
			],
		},
		{
			id: 'safety',
			title: '安全边界',
			hint: '外部输入与扩展（MCP、技能、脚本、网络）的防护上限。调大直接扩大攻击面，请谨慎。',
			fields: [
				{ key: 'mcp_max_binary_payload_bytes', label: 'MCP 二进制内容上限', unit: 'MB', mb: true, danger: true, hint: 'MCP image/audio/resource 内容保留在观察中的 base64 上限，超出替换为 oversized 标记。' },
				{ key: 'mcp_max_sse_buffer_bytes', label: 'MCP SSE 缓冲上限', unit: 'MB', mb: true, danger: true, hint: '单条未完成 SSE 事件缓冲上限，防恶意服务器无限增长。' },
				{ key: 'skills_max_md_bytes', label: 'SKILL.md 大小上限', unit: 'KB', kb: true, danger: true, hint: '超过该大小的技能描述文件被跳过（防 OOM）。' },
				{ key: 'skills_max_parse_lines', label: 'SKILL.md 解析行数', unit: 'lines', danger: true },
				{ key: 'skills_max_line_len', label: 'SKILL.md 单行长度', unit: 'chars', danger: true },
				{ key: 'self_tool_max_instructions_bytes', label: '技能 instructions 上限', unit: 'KB', kb: true, danger: true },
				{ key: 'self_tool_max_script_bytes', label: '技能脚本大小上限', unit: 'KB', kb: true, danger: true },
				{ key: 'network_max_retries', label: '网络重试次数', unit: 'count', danger: true, hint: 'GET 请求的重试次数。调大 = 故障放大。' },
				{ key: 'network_backoff_base_secs', label: '网络重试退避基数', unit: 'secs', danger: false },
				{ key: 'network_max_body_bytes', label: '网络响应体上限', unit: 'MB', mb: true, danger: true },
			],
		},
		{
			id: 'agent',
			title: '代理循环行为',
			hint: 'ReAct 循环的重试/空响应/停滞反馈阈值。这些原本是硬编码常量，现可在设置中调整。',
			fields: [
				{ key: 'cut_off_retries', label: '截断回复重试次数', unit: 'count', danger: false, hint: '看起来被截断/中途停止的文字回复会带提示重试几次再作为最终答案。调大 = 更努力地补全长回复。' },
				{ key: 'empty_response_max_retries', label: '空响应重试次数', unit: 'count', danger: false, hint: '完全空的模型响应重试几次再报错（服务端静默失败的兜底）。' },
				{ key: 'empty_response_retry_delay_ms', label: '空响应重试间隔', unit: 'ms', danger: false },
				{ key: 'stream_stall_warn_delay_ms', label: '流停滞提醒延迟', unit: 'ms', danger: false, hint: '流式输出停顿多久后向界面提示"仍在生成"。调大 = 更晚提示。' },
				{ key: 'reasoning_echo_max_chars', label: '推理回显上限', unit: 'chars', danger: false, hint: '回传给服务商的每轮 reasoning 最大字符数（防止请求体过大导致流中断）。' },
			],
		},
		{
			id: 'resources',
			title: '资源上限',
			hint: '并发与内存资源保护。调大可能造成 CPU/内存/进程占用失控。',
			fields: [
				{ key: 'background_max_actions', label: '后台任务并发上限', unit: 'count', danger: true, hint: '同时运行的 background shell 任务数。调大 = 子进程失控风险。' },
				{ key: 'background_job_tail_max_chars', label: '后台任务输出尾部上限', unit: 'chars', danger: false, hint: '运行中任务实时输出预览保留的尾部字符数。' },
				{ key: 'background_job_output_emit_interval_ms', label: '后台输出事件间隔', unit: 'ms', danger: false },
				{ key: 'terminal_job_ttl_secs', label: '后台任务保留时长', unit: 'secs', danger: false, hint: '已完成任务在面板保留多久后被回收（历史仍存数据库）。' },
				{ key: 'scheduled_actions_max', label: '定时任务数量上限', unit: 'count', danger: true },
				{ key: 'scheduled_actions_due_horizon_secs', label: '定时任务最远排期', unit: 'days', days: true, danger: false },
				{ key: 'clipboard_history_entries', label: '剪贴板历史默认条数', unit: 'count', danger: false },
				{ key: 'clipboard_history_max_entries', label: '剪贴板历史上限', unit: 'count', danger: true },
				{ key: 'clipboard_entry_max_chars', label: '剪贴板条目截断', unit: 'chars', danger: false },
				{ key: 'event_chunk_batch_max_bytes', label: '事件分块批量上限', unit: 'KB', kb: true, danger: false, hint: 'agent 流式事件聚合分块的大小（IPC 频率与延迟权衡）。' },
				{ key: 'input_ring_buffer_secs', label: '音频环形缓冲', unit: 'secs', danger: true, hint: '录音缓冲时长。调大 = 内存增加 + 停止录音后仍会处理更长音频。' },
				{ key: 'embedding_chunk_size', label: '嵌入分块大小', unit: 'count', danger: false, hint: 'embedding 请求分块（提供方限制）。' },
			],
		},
	];

	// Partition each group so `danger` fields sink to the bottom of the card
	// (stable order preserved within each partition) under a collapsible header.
	const LIMIT_VIEWS = LIMIT_GROUPS.map((g) => ({
		...g,
		normal: g.fields.filter((f) => !f.danger),
		danger: g.fields.filter((f) => f.danger),
	}));
	/** @type {Record<string, boolean>} */
	let limitDangerOpen = $state(Object.fromEntries(LIMIT_VIEWS.map((g) => [g.id, true])));
	const isLimitDangerOpen = (/** @type {string} */ id) => limitDangerOpen[id] ?? true;
	let allLimitDangerOpen = $derived(LIMIT_VIEWS.every((g) => !g.danger.length || isLimitDangerOpen(g.id)));
	/**
	 * @param {boolean} open
	 */
	function setAllLimitDanger(open) { for (const g of LIMIT_GROUPS) limitDangerOpen[g.id] = open; }

	/**
	 * @param {string} key
	 * @param {number} value
	 */
	function limitDisplay(key, value) {
		const f = /** @type {any} */ (LIMIT_GROUPS.flatMap((g) => g.fields).find((x) => x.key === key));
		if (!f) return value;
		if (f.mb) return Math.round((value / 1048576) * 10) / 10;
		if (f.kb) return Math.round((value / 1024) * 10) / 10;
		if (f.kTokens) return Math.round(value / 1000);
		if (f.days) return Math.round((value / 86400) * 10) / 10;
		return value;
	}
	/**
	 * @param {string} key
	 * @param {number} v
	 */
	function limitCommit(key, v) {
		const f = /** @type {any} */ (LIMIT_GROUPS.flatMap((g) => g.fields).find((x) => x.key === key));
		if (!f) return v;
		if (f.mb) return Math.round(v * 1048576);
		if (f.kb) return Math.round(v * 1024);
		if (f.kTokens) return Math.round(v * 1000);
		if (f.days) return Math.round(v * 86400);
		return v;
	}

	// Settings sub-tabs: general / models / media / limits.
	// The full `context_limits` object is sent on save so fields the UI
	// does not render are never reset to defaults.
	let settingsTab = $state('general');
	const settingsTabs = [
		{ id: 'general', label: '常规' },
		{ id: 'models', label: '模型' },
		{ id: 'media', label: '媒体' },
		{ id: 'limits', label: '限制' },
	];
	let memory = $state({ session_window_size: 50, history_retention_days: 90 });
	let memoryMaintenance = $state({ running: false, lastCount: null });
	let security = $state({ confirmation_mode: 'ask', min_risk_level: 'medium', permissions: /** @type {Array<{key: string, effect: string}>} */ ([]) });

	let stt = $state({
		provider: 'llm',
		mcp_server: '',
		model: '',
		timeout_secs: 30,
		min_confidence: 0.7,
	});
	let ocr = $state({
		provider: 'llm',
		api_key: '',
		api_secret: '',
		base_url: '',
		timeout_secs: 20,
		min_confidence: 0.7,
	});
	let tts = $state({
		provider: 'none',
		model: '',
		voice: '',
		timeout_secs: 60,
	});
	let imageGen = $state({
		provider: 'none',
		model: '',
		timeout_secs: 120,
	});
	/** @type {{ session_created: { in_app: boolean; windows: boolean }; session_completed: { in_app: boolean; windows: boolean }; session_paused: { in_app: boolean; windows: boolean }; session_resumed: { in_app: boolean; windows: boolean }; session_error: { in_app: boolean; windows: boolean }; [key: string]: { in_app: boolean; windows: boolean } }} */
	let notification = $state({
		session_created: { in_app: true, windows: false },
		session_completed: { in_app: true, windows: true },
		session_paused: { in_app: true, windows: false },
		session_resumed: { in_app: true, windows: false },
		session_error: { in_app: true, windows: true },
	});
	let log = $state({ level: 'info', file_enabled: true });

	// Default shell for the agent's `shell` tool (cmd / Windows PowerShell /
	// PowerShell 7). Availability is probed via check_shell_available so the
	// UI can warn when pwsh is picked without being installed.
	let defaultShell = $state('powershell');
	let shellAvailable = $state({ cmd: true, powershell: true, pwsh: true });
	const SHELL_BASE_OPTIONS = [
		{ value: 'cmd', label: 'cmd.exe（命令提示符）' },
		{ value: 'powershell', label: 'Windows PowerShell（系统自带）' },
		{ value: 'pwsh', label: 'PowerShell 7（pwsh）' },
	];

	function shellOptions() {
		return SHELL_BASE_OPTIONS.map((o) =>
			o.value === 'pwsh' && shellAvailable.pwsh === false
				? { ...o, label: `${o.label}（未安装）` }
				: o,
		);
	}

	async function checkShells() {
		for (const s of ['cmd', 'powershell', 'pwsh']) {
			try {
				const res = parseShellAvailability(await invoke('check_shell_available', { shell: s }));
				shellAvailable[/** @type {'cmd' | 'powershell' | 'pwsh'} */ (s)] = !!res?.available;
			} catch {
				shellAvailable[/** @type {'cmd' | 'powershell' | 'pwsh'} */ (s)] = true; // assume available on probe failure
			}
		}
	}

	// Log viewer (Logging section): reads the tail of the current log file
	// via get_log_info / read_log_tail and shows it in a dialog.
	let logView = $state({ open: false, path: '', content: '', loading: false });
	let logPreEl = /** @type {HTMLPreElement | null} */ ($state(null));

	async function openLogViewer() {
		logView.loading = true;
		try {
			const info = parseLogInfo(await invoke('get_log_info'));
			if (!info?.enabled) {
				addNotification('文件日志未启用，请先打开 File Logging', 'warning', 4000);
				return;
			}
			await refreshLogs();
			logView.open = true;
		} catch (e) {
			addNotification(`无法读取日志: ${formatError(e)}`, 'error', 4000);
		} finally {
			logView.loading = false;
		}
	}

	async function refreshLogs() {
		try {
			const data = parseLogTail(await invoke('read_log_tail', { maxLines: 300 }));
			logView.path = data.path;
			logView.content = data.content;
		} catch (e) {
			addNotification(`无法读取日志: ${formatError(e)}`, 'error', 4000);
		}
	}

	// Keep the viewer pinned to the newest lines whenever content changes.
	$effect(() => {
		if (logView.open && logPreEl) {
			logPreEl.scrollTop = logPreEl.scrollHeight;
		}
	});

	// Names of configured MCP servers, offered in the Audio Model card's
	// Model field when the STT provider is an MCP server.
	/** @type {string[]} */
	let mcpServerNames = $state([]);
	// True once settings + api-key status loaded; passed to ModelSettings so
	// its audio-card STT selector can initialize from the stored config.
	let settingsLoaded = $state(false);

	let accent = $state(themeStore.currentAccent);
	let customAccentHex = $state(themeStore.isPreset ? '#2C5090' : themeStore.accentColor);
	let currentTheme = $state(themeStore.currentTheme);
	const unsubTheme = themeStore.subscribe((v) => { currentTheme = v.theme; });
	// Baseline of persistable form state (after load / successful save). Used
	// to detect unsaved edits when leaving the settings tab.
	/** @type {string} */
	let savedSnapshot = '';
	let leaveDialogOpen = $state(false);
	/** @type {((ok: boolean) => void) | null} */
	let leaveDialogResolve = null;
	let leaveSaving = $state(false);
	// L12: guards against onDestroy running while onMount's async settings
	// load is still in flight.
	let mounted = true;
	/** @type {{ ready: Promise<void>, dispose: () => void } | null} */
	let eventRegistrations = null;
	// Monotonic generation so overlapping syncs never apply an older snapshot
	// after a newer toolbar switch. Self-echo after Save is skipped once.
	let defaultModelSyncGen = 0;
	let skipNextDefaultModelSync = false;
	/** @type {{ model: string, reasoning_effort: string, web_search: string }} */
	let lastSyncedDefaultModel = { model: '', reasoning_effort: '', web_search: 'off' };

	/** @param {any} remote */
	function rememberSyncedDefaultModel(remote) {
		lastSyncedDefaultModel = {
			model: remote?.model || '',
			reasoning_effort: remote?.reasoning_effort || '',
			web_search: remote?.web_search || 'off',
		};
	}

	/** Coerce range/number inputs so DOM string values do not look like edits. */
	/** @param {unknown} v */
	function asNumber(v) {
		const n = Number(v);
		return Number.isFinite(n) ? n : 0;
	}

	/** Serializable subset of form state that Save persists (plus autostart). */
	function buildPersistableSettings() {
		return {
			default_shell: defaultShell,
			llm: llmConfig,
			hotkey: { key_binding: hotkeyBinding, mode: hotkeyMode },
			session: {
				max_concurrent: asNumber(session.max_concurrent),
				max_steps: asNumber(session.max_steps),
			},
			memory: {
				session_window_size: asNumber(memory.session_window_size),
				history_retention_days: asNumber(memory.history_retention_days),
			},
			security: {
				confirmation_mode: security.confirmation_mode,
				min_risk_level: security.min_risk_level,
				permissions: [],
			},
			context_limits: contextLimits,
			media: {
				audio: {
					sample_rate: asNumber(audio.sample_rate),
					channels: asNumber(audio.channels),
					bits_per_sample: asNumber(audio.bits_per_sample),
					max_duration_secs: asNumber(audio.max_duration_secs),
					silence_timeout_ms: asNumber(audio.silence_timeout_ms),
					vad_threshold: asNumber(audio.vad_threshold),
				},
				stt: {
					provider: stt.provider,
					mcp_server: stt.mcp_server || null,
					model: stt.model,
					timeout_secs: asNumber(stt.timeout_secs),
					min_confidence: asNumber(stt.min_confidence),
				},
				ocr: {
					...ocr,
					timeout_secs: asNumber(ocr.timeout_secs),
					min_confidence: asNumber(ocr.min_confidence),
				},
				tts: {
					provider: tts.provider,
					model: tts.model,
					voice: tts.voice,
					timeout_secs: asNumber(tts.timeout_secs),
				},
				image_gen: {
					provider: imageGen.provider,
					model: imageGen.model,
					timeout_secs: asNumber(imageGen.timeout_secs),
				},
			},
			notification: {
				session_created: { ...notification.session_created },
				session_completed: { ...notification.session_completed },
				session_paused: { ...notification.session_paused },
				session_resumed: { ...notification.session_resumed },
				session_error: { ...notification.session_error },
			},
			log: { level: log.level, file_enabled: log.file_enabled },
			autostart_enabled: autostartEnabled,
			key_configured: { ...keyConfigured },
			key_configured_providers: { ...keyConfiguredProviders },
		};
	}

	function captureSnapshot() {
		savedSnapshot = JSON.stringify(buildPersistableSettings());
	}

	/** Lazy dirty check — only run on leave, not on every keystroke. */
	function isDirty() {
		return settingsLoaded && !!savedSnapshot && JSON.stringify(buildPersistableSettings()) !== savedSnapshot;
	}

	/**
	 * Discovery filled empty Context/cost on some roles. Patch only those
	 * keys into the dirty snapshot so a user Context/cost edit stays dirty
	 * and still gets the leave prompt.
	 * @param {{ role: string, context_window?: unknown, cost_per_1k_input_tokens?: unknown, cost_per_1k_output_tokens?: unknown }[]} [fills]
	 */
	function reBaselineAfterDiscovery(fills) {
		if (!mounted || !settingsLoaded || !savedSnapshot) return;
		if (!Array.isArray(fills) || fills.length === 0) return;
		try {
			const snap = JSON.parse(savedSnapshot);
			const roles = Array.isArray(snap?.llm?.roles) ? snap.llm.roles : [];
			for (const fill of fills) {
				const idx = roles.findIndex((/** @type {any} */ r) => r.role === fill.role);
				if (idx < 0) continue;
				if ('context_window' in fill) roles[idx].context_window = fill.context_window;
				if ('cost_per_1k_input_tokens' in fill) {
					roles[idx].cost_per_1k_input_tokens = fill.cost_per_1k_input_tokens;
				}
				if ('cost_per_1k_output_tokens' in fill) {
					roles[idx].cost_per_1k_output_tokens = fill.cost_per_1k_output_tokens;
				}
			}
			if (!snap.llm) snap.llm = {};
			snap.llm.roles = roles;
			savedSnapshot = JSON.stringify(snap);
		} catch (e) {
			logger.warn('SettingsView', 're-baseline after discovery failed', e);
		}
	}

	/**
	 * Align the dirty baseline's default_model role with a toolbar-driven
	 * remote sync so it does not look like a local unsaved edit.
	 * @param {any} remote
	 */
	function patchSnapshotDefaultModel(remote) {
		if (!savedSnapshot || !remote) return;
		try {
			const snap = JSON.parse(savedSnapshot);
			const roles = Array.isArray(snap?.llm?.roles) ? snap.llm.roles : [];
			const idx = roles.findIndex((/** @type {any} */ r) => r.role === 'default_model');
			const patched = {
				...(idx >= 0 ? roles[idx] : { role: 'default_model' }),
				provider: remote.provider,
				model: remote.model,
				reasoning_effort: remote.reasoning_effort,
				web_search: remote.web_search,
			};
			if (idx >= 0) roles[idx] = patched;
			else roles.push(patched);
			if (!snap.llm) snap.llm = {};
			snap.llm.roles = roles;
			savedSnapshot = JSON.stringify(snap);
		} catch (e) {
			logger.warn('SettingsView', 'patch snapshot default_model failed', e);
		}
	}

	/** Restore form fields from the last saved/loaded snapshot (discard edits). */
	function discardChanges() {
		if (!savedSnapshot) return;
		try {
			const snap = JSON.parse(savedSnapshot);
			defaultShell = snap.default_shell || defaultShell;
			if (snap.llm) {
				llmConfig = {
					...llmConfig,
					...snap.llm,
					providers: Array.isArray(snap.llm.providers) ? snap.llm.providers : [],
					roles: Array.isArray(snap.llm.roles) ? snap.llm.roles : [],
				};
				rememberSyncedDefaultModel(
					llmConfig.roles.find((/** @type {any} */ r) => r.role === 'default_model'),
				);
			}
			if (snap.hotkey) {
				hotkeyBinding = snap.hotkey.key_binding || hotkeyBinding;
				hotkeyMode = snap.hotkey.mode || hotkeyMode;
			}
			if (snap.session) session = { ...session, ...snap.session };
			if (snap.memory) memory = { ...memory, ...snap.memory };
			if (snap.security) {
				const mode = snap.security.confirmation_mode || security.confirmation_mode;
				security = {
					confirmation_mode: mode,
					min_risk_level: snap.security.min_risk_level || security.min_risk_level,
					permissions: Array.isArray(snap.security.permissions)
						? snap.security.permissions
						: security.permissions,
				};
			}
			if (snap.context_limits) contextLimits = { ...contextLimits, ...snap.context_limits };
			if (snap.media?.audio) audio = { ...audio, ...snap.media.audio };
			if (snap.media?.stt) {
				stt = {
					provider: snap.media.stt.provider || 'llm',
					mcp_server: snap.media.stt.mcp_server || '',
					model: snap.media.stt.model || '',
					timeout_secs: snap.media.stt.timeout_secs || 30,
					min_confidence: snap.media.stt.min_confidence ?? 0.7,
				};
			}
			if (snap.media?.ocr) ocr = { ...ocr, ...snap.media.ocr };
			if (snap.media?.tts) {
				tts = {
					provider: snap.media.tts.provider || 'none',
					model: snap.media.tts.model || '',
					voice: snap.media.tts.voice || '',
					timeout_secs: snap.media.tts.timeout_secs || 60,
				};
			}
			if (snap.media?.image_gen) {
				imageGen = {
					provider: snap.media.image_gen.provider || 'none',
					model: snap.media.image_gen.model || '',
					timeout_secs: snap.media.image_gen.timeout_secs || 120,
				};
			}
			if (snap.notification) {
				notification = {
					session_created: { ...notification.session_created, ...snap.notification.session_created },
					session_completed: { ...notification.session_completed, ...snap.notification.session_completed },
					session_paused: { ...notification.session_paused, ...snap.notification.session_paused },
					session_resumed: { ...notification.session_resumed, ...snap.notification.session_resumed },
					session_error: { ...notification.session_error, ...snap.notification.session_error },
				};
			}
			if (snap.log) log = { ...log, level: snap.log.level, file_enabled: snap.log.file_enabled };
			if (typeof snap.autostart_enabled === 'boolean') autostartEnabled = snap.autostart_enabled;
			if (snap.key_configured && typeof snap.key_configured === 'object') {
				keyConfigured = { ...keyConfigured, ...snap.key_configured };
			}
			if (snap.key_configured_providers && typeof snap.key_configured_providers === 'object') {
				keyConfiguredProviders = { ...snap.key_configured_providers };
			}
			captureSnapshot();
		} catch (e) {
			logger.warn('SettingsView', 'discard changes failed', e);
		}
	}

	/**
	 * @returns {Promise<boolean>}
	 */
	function confirmLeave() {
		if (leaveDialogOpen) {
			return new Promise((resolve) => {
				const prev = leaveDialogResolve;
				leaveDialogResolve = (ok) => {
					prev?.(false);
					resolve(ok);
				};
			});
		}
		leaveDialogOpen = true;
		return new Promise((resolve) => {
			leaveDialogResolve = resolve;
		});
	}

	/** @param {boolean} ok */
	function finishLeaveDialog(ok) {
		leaveDialogOpen = false;
		leaveSaving = false;
		const resolve = leaveDialogResolve;
		leaveDialogResolve = null;
		resolve?.(ok);
	}

	function leaveWithoutSaving() {
		discardChanges();
		finishLeaveDialog(true);
	}

	async function leaveWithSaving() {
		leaveSaving = true;
		const ok = await saveSettings();
		if (ok) finishLeaveDialog(true);
		else {
			leaveSaving = false;
			// Stay on settings; keep the dialog open so the user can retry or cancel.
		}
	}

	function stayOnSettings() {
		if (leaveSaving) return;
		finishLeaveDialog(false);
	}

	/** @param {any} remote */
	function applyRemoteDefaultModelFields(remote) {
		if (!remote) return;
		/** @type {any} */
		const local = (Array.isArray(llmConfig.roles) ? llmConfig.roles : []).find(
			(/** @type {any} */ r) => r.role === 'default_model',
		);
		if (local) {
			// Mutate in place (Svelte 5 $state proxy) so other unsaved
			// role/provider edits on this form are preserved.
			local.provider = remote.provider;
			local.model = remote.model;
			local.reasoning_effort = remote.reasoning_effort;
			local.web_search = remote.web_search;
		} else if (Array.isArray(llmConfig.roles)) {
			/** @type {any[]} */ (llmConfig.roles).push(remote);
		}
		rememberSyncedDefaultModel(remote);
		patchSnapshotDefaultModel(remote);
	}

	/**
	 * Keep the default_model role row in sync when the chat toolbar switches
	 * model / effort / web search. Only those fields are overwritten so
	 * unsaved edits on other roles/providers survive the event. Keep-alive
	 * leaves this view mounted across tab switches, so mount-time load alone
	 * is not enough.
	 */
	async function syncDefaultModelRoleFromBackend() {
		const gen = ++defaultModelSyncGen;
		try {
			const settings = await invoke('get_settings');
			if (!mounted || gen !== defaultModelSyncGen || !settings?.llm) return;
			const remoteRoles = Array.isArray(settings.llm.roles) ? settings.llm.roles : [];
			const remote = remoteRoles.find((/** @type {any} */ r) => r.role === 'default_model');
			if (!remote) return;
			applyRemoteDefaultModelFields(remote);
		} catch (e) {
			logger.warn('SettingsView', 'sync default_model role error', e);
		}
	}

	/**
	 * Before Save, adopt backend default_model fields the user did not edit
	 * on this form — so a toolbar switch cannot be clobbered by a stale row.
	 * Fields the user changed since last sync keep the form values.
	 */
	async function reconcileDefaultModelBeforeSave() {
		try {
			const settings = await invoke('get_settings');
			if (!mounted || !settings?.llm) return;
			const remote = (Array.isArray(settings.llm.roles) ? settings.llm.roles : []).find(
				(/** @type {any} */ r) => r.role === 'default_model',
			);
			/** @type {any} */
			const local = (Array.isArray(llmConfig.roles) ? llmConfig.roles : []).find(
				(/** @type {any} */ r) => r.role === 'default_model',
			);
			if (!remote || !local) return;
			if ((local.model || '') === lastSyncedDefaultModel.model) {
				local.model = remote.model;
			}
			if ((local.reasoning_effort || '') === lastSyncedDefaultModel.reasoning_effort) {
				local.reasoning_effort = remote.reasoning_effort;
			}
			if ((local.web_search || 'off') === lastSyncedDefaultModel.web_search) {
				local.web_search = remote.web_search;
			}
			rememberSyncedDefaultModel(local);
		} catch (e) {
			logger.warn('SettingsView', 'reconcile default_model before save failed', e);
		}
	}

	onDestroy(() => {
		mounted = false;
		unsubTheme();
		eventRegistrations?.dispose();
		eventRegistrations = null;
		registerSettingsLeaveGuard(null);
		if (leaveDialogResolve) {
			leaveDialogResolve(false);
			leaveDialogResolve = null;
		}
	});

	onMount(async () => {
		registerSettingsLeaveGuard({ isDirty, confirmLeave });
		eventRegistrations = registerListeners(
			{
				'llm:config_changed': () => {
					if (skipNextDefaultModelSync) {
						skipNextDefaultModelSync = false;
						return;
					}
					syncDefaultModelRoleFromBackend();
				},
			},
			{ tag: 'SettingsView' },
		);
		try {
			const settings = await invoke('get_settings');
			if (!mounted) return;
			if (settings) {
				llmConfig = settings.llm || llmConfig;
				// Normalize shape: the backend never sends secrets, but ensure
				// the arrays exist so downstream UI code can always iterate.
				llmConfig.providers = Array.isArray(llmConfig.providers) ? llmConfig.providers : [];
				llmConfig.roles = Array.isArray(llmConfig.roles) ? llmConfig.roles : [];
				// Materialize all role slots before the dirty baseline — otherwise
				// ModelSettings' mount $effect appending empty slots looks like an edit.
				ensureRoleSlots(llmConfig.roles);
				rememberSyncedDefaultModel(
					llmConfig.roles.find((/** @type {any} */ r) => r.role === 'default_model'),
				);
				hotkeyBinding = settings.hotkey?.key_binding || hotkeyBinding;
				hotkeyMode = settings.hotkey?.mode || 'toggle';
				session = settings.session || session;
				contextLimits = settings.context_limits || contextLimits;
				memory = settings.memory || memory;
				{
					const mode = settings.security?.confirmation_mode || 'ask';
					security = {
						confirmation_mode: mode,
						min_risk_level: settings.security?.min_risk_level || 'medium',
						permissions: Array.isArray(settings.security?.permissions)
							? settings.security.permissions
							: [],
					};
				}
				const media = settings.media || {};
				audio = media.audio || audio;
				stt = {
					provider: media.stt?.provider || 'llm',
					mcp_server: media.stt?.mcp_server || '',
					model: media.stt?.model || '',
					timeout_secs: media.stt?.timeout_secs || 30,
					min_confidence: media.stt?.min_confidence ?? 0.7,
				};
				ocr = {
					provider: media.ocr?.provider || 'llm',
					api_key: media.ocr?.api_key || '',
					api_secret: media.ocr?.api_secret || '',
					base_url: media.ocr?.base_url || '',
					timeout_secs: media.ocr?.timeout_secs || 20,
					min_confidence: media.ocr?.min_confidence ?? 0.7,
				};
				tts = {
					provider: media.tts?.provider || 'none',
					model: media.tts?.model || '',
					voice: media.tts?.voice || '',
					timeout_secs: media.tts?.timeout_secs || 60,
				};
				imageGen = {
					provider: media.image_gen?.provider || 'none',
					model: media.image_gen?.model || '',
					timeout_secs: media.image_gen?.timeout_secs || 120,
				};
				// MCP server names for the Audio Model card's MCP STT mode.
				mcpServerNames = (settings.mcp_servers || []).map((/** @type {any} */ s) => s.name || '').filter(Boolean);
				notification = settings.notification || notification;
				log = settings.log || log;
				defaultShell = settings.default_shell || 'powershell';
				checkShells();
			}
		} catch (e) {
			addNotification(`加载设置失败: ${formatError(e)}`, 'error', 4000);
		}
		// Key status must resolve before settingsLoaded flips: ModelSettings
		// auto-fetches /models on load, and Provider cards must not show
		// 「未配置」while waiting on a large model list.
		try {
			await refreshApiKeyStatus();
			if (!mounted) return;
		} catch (e) {
			addNotification(`获取 API Key 状态失败: ${formatError(e)}`, 'error', 3000);
		}
		if (mounted) settingsLoaded = true;
		try {
			autostartEnabled = await invoke('is_autostart_enabled');
			if (!mounted) return;
		} catch (e) {
			addNotification(`获取开机自启状态失败: ${formatError(e)}`, 'error', 3000);
		}
		// Let ModelSettings mount effects settle before baselining dirty state.
		if (mounted) {
			await tick();
			if (mounted) captureSnapshot();
		}
	});

	async function runMaintenance() {
		memoryMaintenance.running = true;
		try {
			memoryMaintenance.lastCount = await invoke('run_memory_maintenance');
			addNotification(`记忆维护完成（清理 ${memoryMaintenance.lastCount} 项）`, 'success', 3000);
		} catch (e) {
			addNotification(`记忆维护失败: ${formatError(e)}`, 'error', 4000);
		} finally {
			memoryMaintenance.running = false;
		}
	}

	async function refreshApiKeyStatus() {
		const { providers, ...flags } = parseApiKeyStatus(await invoke('get_api_key_status'));
		// Keep role/media flags as a flat boolean map; never assign the raw
		// payload (it nests `providers`) into keyConfigured.
		keyConfigured = { ...keyConfigured, ...flags };
		keyConfiguredProviders = { ...providers };
	}

	/** @returns {Promise<boolean>} true when the settings payload was saved */
	async function saveSettings() {
		try {
			await reconcileDefaultModelBeforeSave();
			skipNextDefaultModelSync = true;
			await invoke('update_settings', {
				settings: {
					default_shell: defaultShell,
					llm: llmConfig,
					hotkey: { key_binding: hotkeyBinding, mode: hotkeyMode, mute_hotkey: null },
					session: {
						max_concurrent: session.max_concurrent,
						max_steps: session.max_steps,
					},
					memory: {
						session_window_size: memory.session_window_size,
						history_retention_days: memory.history_retention_days,
					},
					security: {
						confirmation_mode: security.confirmation_mode,
						min_risk_level: security.min_risk_level,
						// permissions / encrypt_sensitive are owned by
						// resolve_confirmation / revoke_permission / disk —
						// loader preserves them on save.
						permissions: [],
					},
					// Full object (loaded state kept intact) so fields the UI does
					// not render are preserved; backend applies it wholesale.
					context_limits: contextLimits,
					media: {
						audio: {
							sample_rate: audio.sample_rate,
							channels: audio.channels,
							bits_per_sample: audio.bits_per_sample,
							max_duration_secs: audio.max_duration_secs,
							silence_timeout_ms: audio.silence_timeout_ms,
							vad_threshold: audio.vad_threshold,
						},
						stt: {
							provider: stt.provider,
							mcp_server: stt.mcp_server || null,
							model: stt.model,
							timeout_secs: stt.timeout_secs,
							min_confidence: stt.min_confidence,
						},
						ocr: {
							provider: ocr.provider,
							api_key: ocr.api_key,
							api_secret: ocr.api_secret,
							base_url: ocr.base_url,
							timeout_secs: ocr.timeout_secs,
							min_confidence: ocr.min_confidence,
						},
						tts: {
							provider: tts.provider,
							model: tts.model,
							voice: tts.voice,
							timeout_secs: tts.timeout_secs,
						},
						image_gen: {
							provider: imageGen.provider,
							model: imageGen.model,
							timeout_secs: imageGen.timeout_secs,
						},
					},
					notification: {
						session_created: { in_app: notification.session_created.in_app, windows: notification.session_created.windows },
						session_completed: { in_app: notification.session_completed.in_app, windows: notification.session_completed.windows },
						session_paused: { in_app: notification.session_paused.in_app, windows: notification.session_paused.windows },
						session_resumed: { in_app: notification.session_resumed.in_app, windows: notification.session_resumed.windows },
						session_error: { in_app: notification.session_error.in_app, windows: notification.session_error.windows },
					},
			log: {
				level: log.level,
				file_enabled: log.file_enabled,
				file_path: null,
			},
				},
			});
		addNotification('设置已保存', 'success');
			try {
				await refreshApiKeyStatus();
			} catch (e) {
				addNotification(`获取 API Key 状态失败: ${formatError(e)}`, 'error', 3000);
			}
			if (autostartEnabled) {
				try { await invoke('enable_autostart'); } catch (e) {
					autostartEnabled = false;
					addNotification(`自动启动：${formatError(e)}`, 'warning');
				}
			} else {
				try { await invoke('disable_autostart'); } catch (e) {
					autostartEnabled = true;
					addNotification(`取消自动启动：${formatError(e)}`, 'warning');
				}
			}
			if (mounted) captureSnapshot();
			return true;
		} catch (e) {
			// Save never emitted llm:config_changed — clear the skip so the
			// next real toolbar/config event is not swallowed.
			skipNextDefaultModelSync = false;
			addNotification(`保存设置失败: ${formatError(e)}`, 'error', 5000);
			return false;
		}
	}

	async function toggleAutostart() {
		autostartEnabled = !autostartEnabled;
	}

	/**
	 * @param {string} hex
	 */
	function contrastText(hex) {
		const r = parseInt(hex.slice(1, 3), 16);
		const g = parseInt(hex.slice(3, 5), 16);
		const b = parseInt(hex.slice(5, 7), 16);
		const lum = (0.299 * r + 0.587 * g + 0.114 * b) / 255;
		return lum > 0.5 ? '#000000' : '#ffffff';
	}

</script>

<div class="settings-page">
	<h1>Settings</h1>

	<div class="md-tabs settings-tabs" role="tablist">
		{#each settingsTabs as tab}
			<button
				class="md-tab"
				class:active={settingsTab === tab.id}
				role="tab"
				aria-selected={settingsTab === tab.id}
				onclick={() => (settingsTab = tab.id)}
			>
				{tab.label}
			</button>
		{/each}
	</div>

	{#if settingsTab === 'general'}

	<div class="section">
		<h2>Hotkeys</h2>
		<div class="form-row">
			<label for="hotkey-binding">Key Binding</label>
			<HotkeyInput id="hotkey-binding" value={hotkeyBinding} onChange={(/** @type {string} */ v) => { hotkeyBinding = v; }} />
		</div>
		<div class="form-row">
			<label for="hotkey-mode">Mode</label>
			<MaterialSelect id="hotkey-mode" value={hotkeyMode} options={[{ value: 'toggle', label: 'Toggle (press to start/stop)' }, { value: 'hold', label: 'Hold (push-to-talk)' }]} onChange={(/** @type {string} */ v) => { hotkeyMode = v; }} />
		</div>
	</div>

	<div class="section">
		<h2>Session &amp; Concurrency</h2>
		<p class="model-hint">Max Concurrent 控制同时运行的会话数；LLM Per-Endpoint Concurrency 限制每个模型端点（角色）同时在途的请求数。后者低于前者时，超出上限的模型请求会排队等待，避免多个会话同时请求同一服务商触发限流（429）。</p>
		<div class="form-row">
			<label for="session-max-concurrent">Max Concurrent</label>
			<MaterialNumberField id="session-max-concurrent" value={session.max_concurrent} min={1} max={10} onChange={(/** @type {number} */ v) => { session.max_concurrent = v; }} />
		</div>
		<div class="form-row">
			<label for="llm-max-concurrent-requests">LLM Per-Endpoint Concurrency</label>
			<MaterialNumberField id="llm-max-concurrent-requests" value={llmConfig.max_concurrent_requests} min={1} max={16} onChange={(/** @type {number} */ v) => { llmConfig.max_concurrent_requests = v; }} />
		</div>
		<div class="form-row">
			<label for="session-max-steps">Max Steps</label>
			<MaterialNumberField id="session-max-steps" value={session.max_steps} min={1} max={100} onChange={(/** @type {number} */ v) => { session.max_steps = v; }} />
		</div>
	</div>

	<div class="section">
		<h2>Agent Shell</h2>
		<p class="model-hint">Agent 的 shell 工具默认使用的命令行解释器。模型仍可在调用时通过 shell 参数临时指定其他 shell（cmd / powershell / pwsh）。</p>
		<div class="form-row">
			<label for="default-shell">Default Shell</label>
			<MaterialSelect id="default-shell" value={defaultShell} options={shellOptions()} onChange={(/** @type {string} */ v) => { defaultShell = v; }} />
		</div>
		{#if defaultShell === 'pwsh' && shellAvailable.pwsh === false}
			<div class="shell-warning">
				<p>未检测到 PowerShell 7（pwsh），命令将无法执行。请先安装：</p>
				<code>winget install Microsoft.PowerShell</code>
			</div>
		{/if}
	</div>

	<div class="section">
		<h2>Memory</h2>
		<div class="form-row">
			<label for="memory-window-size">Window Size</label>
			<MaterialNumberField id="memory-window-size" value={memory.session_window_size} min={10} max={500} onChange={(/** @type {number} */ v) => { memory.session_window_size = v; }} />
		</div>
		<div class="form-row">
			<label for="memory-retention">Retention (days)</label>
			<MaterialNumberField id="memory-retention" value={memory.history_retention_days} min={1} max={365} onChange={(/** @type {number} */ v) => { memory.history_retention_days = v; }} />
		</div>
		<h3 class="model-group-heading">Maintenance</h3>
		<p class="model-hint">维护会清理重复、敏感、过期的事实与残留向量。</p>
		<div class="form-row">
			<button class="md-btn" onclick={runMaintenance} disabled={memoryMaintenance.running}>
				{memoryMaintenance.running ? 'Running…' : 'Run Memory Maintenance'}
			</button>
			{#if memoryMaintenance.lastCount !== null}
				<span class="recall-hint">上次清理 {memoryMaintenance.lastCount} 项</span>
			{/if}
		</div>
	</div>

	<div class="section appearance-section">
		<h2>Appearance</h2>
		<div class="form-row">
			<span class="form-label">Theme</span>
			<div class="theme-toggle-row" role="radiogroup" aria-label="Theme">
				<button
					class="md-btn"
					class:md-btn--outlined={currentTheme === 'light'}
					class:md-btn--filled={currentTheme !== 'light'}
					role="radio"
					aria-checked={currentTheme === 'light'}
				onclick={() => { themeStore.setTheme('light'); }}
			>Light</button>
			<button
				class="md-btn"
				class:md-btn--outlined={currentTheme === 'dark'}
				class:md-btn--filled={currentTheme !== 'dark'}
				role="radio"
				aria-checked={currentTheme === 'dark'}
				onclick={() => { themeStore.setTheme('dark'); }}
			>Dark</button>
			</div>
		</div>
		<div class="form-row">
			<span class="form-label">Accent Color</span>
			<div class="accent-picker" role="radiogroup" aria-label="Accent color">
				{#each Object.entries(themeStore.presets) as [key, preset]}
					<button
						class="md-btn"
						class:accent-swatch-selected={accent === key}
						style="background: {preset.hex}; color: {contrastText(preset.hex)}; --_btn-state: {contrastText(preset.hex)}; border: 2px solid transparent; border-color: {accent === key ? contrastText(preset.hex) : 'transparent'}"
						role="radio"
						aria-checked={accent === key}
						aria-label="{preset.label} {preset.hex}"
						onclick={() => { accent = key; themeStore.setAccent(key); }}
					>{preset.label}</button>
				{/each}
				<button
					class="md-btn md-btn--filled"
					class:md-btn--outlined={accent.startsWith('#') || accent.startsWith('custom:')}
					role="radio"
					aria-checked={accent.startsWith('#') || accent.startsWith('custom:')}
					aria-label="Custom hex color"
				>
					<input
						id="custom-accent"
						type="text"
						class="custom-hex-input"
						placeholder="#RRGGBB"
						maxlength="7"
						value={customAccentHex}
						autocomplete="off"
						oninput={(e) => {
							const val = /** @type {HTMLInputElement} */(e.target).value;
							customAccentHex = val;
							if (/^#[0-9a-f]{6}$/i.test(val)) {
								accent = val;
								themeStore.setAccent(val);
							}
						}}
					/>
				</button>
			</div>
		</div>
	</div>

	<div class="section">
		<h2>Security</h2>
		<div class="form-row">
			<label for="security-mode">Confirmation Mode</label>
			<MaterialSelect id="security-mode" value={security.confirmation_mode} options={[
				{ value: 'ask', label: 'Ask (by risk threshold)' },
				{ value: 'paranoid', label: 'Paranoid (all non-safe)' },
				{ value: 'autopilot', label: 'Autopilot (never ask)' },
			]} onChange={(/** @type {string} */ v) => { security.confirmation_mode = v; }} />
		</div>
		<div class="form-row">
			<label for="security-min-level">Minimum Confirmation Level</label>
			<MaterialSelect id="security-min-level" value={security.min_risk_level} options={[
				{ value: 'safe', label: 'None (all auto-approved)' },
				{ value: 'low', label: 'Low & above' },
				{ value: 'medium', label: 'Medium & above' },
				{ value: 'high', label: 'High & above' },
				{ value: 'critical', label: 'Critical only' },
			]} onChange={(/** @type {string} */ v) => { security.min_risk_level = v; }} />
		</div>
		<p class="model-hint">仅 Ask 模式使用风险阈值。永久允许/拒绝优先于阈值；禁用操作与路径沙箱始终拦截。Autopilot 仍会执行永久拒绝。</p>
		{#if security.permissions.length > 0}
			<div class="perm-list">
				<div class="perm-list-title">永久权限</div>
				{#each security.permissions as perm (perm.key)}
					<div class="perm-row">
						<code class="perm-key">{perm.key}</code>
						<span class="perm-effect" class:deny={perm.effect === 'deny'}>{perm.effect === 'deny' ? '拒绝' : '允许'}</span>
						<button
							type="button"
							class="perm-revoke"
							onclick={async () => {
								try {
									await invoke('revoke_permission', { key: perm.key });
									security.permissions = security.permissions.filter((p) => p.key !== perm.key);
								} catch (e) {
									addNotification(`撤销权限失败: ${e}`, 'error', 3000);
								}
							}}
						>撤销</button>
					</div>
				{/each}
			</div>
		{:else}
			<p class="model-hint">暂无永久权限。确认弹窗中选「始终允许 / 始终拒绝」后会出现在这里。</p>
		{/if}
	</div>

	<div class="section notification-section">
		<h2>Notifications</h2>
		<div class="notify-grid-header">
			<span class="switch-label"></span>
			<span class="switch-label">In-App Toast</span>
			<span class="switch-label">Windows</span>
		</div>
		{#each [
			{ key: 'session_created', label: 'Session Start' },
			{ key: 'session_completed', label: 'Session Complete' },
			{ key: 'session_paused', label: 'Session Paused' },
			{ key: 'session_resumed', label: 'Session Resumed' },
			{ key: 'session_error', label: 'Session Error' },
		] as ev (ev.key)}
			<div class="notify-grid-row">
				<span class="switch-label">{ev.label}</span>
				<MaterialSwitch checked={notification[ev.key].in_app} onChange={(/** @type {boolean} */ v) => { notification[ev.key].in_app = v; }} />
				<MaterialSwitch checked={notification[ev.key].windows} onChange={(/** @type {boolean} */ v) => { notification[ev.key].windows = v; }} />
			</div>
		{/each}
		<p class="model-hint">Agent 通过 notify 工具发出的通知始终开启（应用内 + Windows），不受上表开关控制。</p>
	</div>

	<div class="section log-section">
		<div class="llm-head">
			<h2>Logging</h2>
			<button class="md-btn md-btn--outlined" onclick={openLogViewer} disabled={logView.loading}>查看日志</button>
		</div>
		<div class="form-row switch-row">
			<span class="switch-label">File Logging</span>
			<MaterialSwitch checked={log.file_enabled} onChange={(/** @type {boolean} */ v) => { log.file_enabled = v; }} />
		</div>
		<div class="form-row">
			<label for="log-level">Log Level</label>
			<MaterialSelect id="log-level" value={log.level} options={[
				{ value: 'trace', label: 'Trace' },
				{ value: 'debug', label: 'Debug' },
				{ value: 'info', label: 'Info' },
				{ value: 'warn', label: 'Warn' },
				{ value: 'error', label: 'Error' },
			]} onChange={(/** @type {string} */ v) => { log.level = v; }} />
		</div>
		<p class="model-hint">日志级别与文件输出仅作用于后端（tracing）；前端开发日志仍按 DEV/PROD 门控。</p>
	</div>

	<div class="section autostart-section">
		<h2>Autostart</h2>
		<div class="form-row autostart-row">
			<span class="autostart-label">Launch Haven on system startup</span>
				<MaterialSwitch checked={autostartEnabled} onChange={toggleAutostart} />
		</div>
	</div>
	{/if}

	{#if settingsTab === 'models' || settingsTab === 'media'}
	{#if settingsLoaded}
	<ModelSettings
		section={settingsTab}
		{llmConfig}
		{audio}
		{stt}
		{ocr}
		{tts}
		{imageGen}
		{contextLimits}
		{keyConfigured}
		{keyConfiguredProviders}
		{mcpServerNames}
		loaded={true}
		onDiscoverySettled={reBaselineAfterDiscovery}
	/>
	{:else}
	<p class="model-hint">正在加载模型与 API Key 状态…</p>
	{/if}
	{/if}

	{#snippet limitRow(/** @type {any} */ f, boxed = false)}
	<div class="form-row limit-row" class:danger-row={f.danger && !boxed}>
		<div class="limit-label">
			<label for="limit-{f.key}">{f.label}</label>
			{#if f.danger && !boxed}
			<span class="danger-badge" title={f.hint || '调整此值存在内存 / 成本 / 安全风险'}>⚠ 危险</span>
			{/if}
			{#if f.hint}
			<p class="limit-hint">{f.hint}</p>
			{/if}
		</div>
		<div class="limit-input">
			<MaterialNumberField
				id="limit-{f.key}"
				value={limitDisplay(f.key, contextLimits[f.key])}
				step={f.step ?? 1}
				min={f.min ?? 0}
				max={f.max ?? 100000000}
				onChange={(/** @type {number} */ v) => { contextLimits[f.key] = limitCommit(f.key, v); }}
			/>
			<span class="limit-unit">{f.unit}</span>
		</div>
	</div>
	{/snippet}

	{#if settingsTab === 'limits'}
	<div class="limits-toolbar">
		<p class="limits-legend">红色边框为<b>危险项</b>：调大会扩大内存 / 成本 / 攻击面，默认排在每组底部，可折叠。</p>
		<button class="md-btn md-btn--text limit-toggle-all" onclick={() => setAllLimitDanger(!allLimitDangerOpen)} aria-expanded={allLimitDangerOpen}>
			<span class="limit-danger-caret" aria-hidden="true"><svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="6 9 12 15 18 9" /></svg></span>
			{allLimitDangerOpen ? '折叠全部危险项' : '展开全部危险项'}
		</button>
	</div>
	<div class="limits-grid">
		{#each LIMIT_VIEWS as group}
		<div class="format-card limit-card">
			<h3>{group.title}</h3>
			<p class="model-hint">{group.hint}</p>
			{#each group.normal as f}
			{@render limitRow(f)}
			{/each}
			{#if group.danger.length}
			<div class="limit-danger-box">
				<MaterialCollapsible variant="error" bind:open={limitDangerOpen[group.id]}>
					{#snippet header()}
						<span class="danger-badge">⚠ 危险项</span>
						<span class="limit-danger-count">{group.danger.length} 项</span>
					{/snippet}
					<div class="limit-danger-items">
					{#each group.danger as f}
					{@render limitRow(f, true)}
					{/each}
					</div>
				</MaterialCollapsible>
			</div>
			{/if}
		</div>
		{/each}
	</div>
	{/if}

	<button class="md-btn md-btn--filled save-btn" onclick={saveSettings}>
		Save Settings
	</button>
</div>

{#if logView.open}
<MaterialDialog
	open={true}
	title="日志查看"
	dialogClass="md-dialog--wide"
	onClose={() => { logView.open = false; }}
>
	{#snippet children()}
		{#if logView.path}
			<p class="log-path" title={logView.path}>{logView.path}</p>
		{/if}
		<pre class="log-viewer" bind:this={logPreEl}>{logView.content || '（暂无日志内容）'}</pre>
	{/snippet}
	{#snippet footer()}
		<button class="md-btn md-btn--outlined" onclick={refreshLogs} disabled={logView.loading}>刷新</button>
		<button class="md-btn" onclick={() => { logView.open = false; }}>关闭</button>
	{/snippet}
</MaterialDialog>
{/if}

<MaterialDialog
	open={leaveDialogOpen}
	title="未保存的更改"
	onClose={stayOnSettings}
>
	{#snippet children()}
		<p>设置已修改但尚未保存。选择「取消」将放弃更改并离开；或先保存再离开。</p>
	{/snippet}
	{#snippet footer()}
		<button class="md-btn md-btn--text" onclick={leaveWithoutSaving} disabled={leaveSaving}>取消</button>
		<button class="md-btn md-btn--filled" onclick={leaveWithSaving} disabled={leaveSaving}>
			{leaveSaving ? '保存中…' : '保存并离开'}
		</button>
	{/snippet}
</MaterialDialog>

<style>
	.settings-page { max-width: var(--md-sys-content-max-width); }
	.settings-tabs { margin-bottom: var(--md-sys-space-xl); }
	.format-card {
		background: var(--md-sys-color-surface-container-lowest);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		padding: var(--md-sys-space-md);
		margin-bottom: var(--md-sys-space-md);
	}
	.format-card h3 { font-size: 14px; font-weight: 600; color: var(--md-sys-color-primary); margin-bottom: var(--md-sys-space-sm); }
	.format-card .model-hint { margin-top: 0; margin-bottom: var(--md-sys-space-md); }
	.limits-grid { display: grid; grid-template-columns: 1fr 1fr; gap: var(--md-sys-space-md); }
	.limit-card { min-width: 0; }
	.limits-toolbar {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
		flex-wrap: wrap;
		margin-bottom: var(--md-sys-space-md);
	}
	.limits-legend { font-size: 12px; color: var(--md-sys-color-on-surface-variant); margin: 0; }
	.limits-legend b { color: var(--md-sys-color-error, #ba1a1a); font-weight: 600; }
	.limit-row { display: flex; justify-content: space-between; align-items: flex-start; gap: var(--md-sys-space-md); }
	.limit-danger-box {
		border: 1px solid var(--md-sys-color-error, #ba1a1a);
		border-radius: var(--md-sys-shape-small);
		background: color-mix(in srgb, var(--md-sys-color-error, #ba1a1a) 6%, transparent);
		margin-top: var(--md-sys-space-sm);
		padding: 8px;
	}
	.limit-danger-items { display: grid; gap: 10px; }
	.limit-danger-caret {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		flex-shrink: 0;
		color: var(--md-sys-color-error, #ba1a1a);
		transition: transform 0.15s ease;
	}
	.limit-toggle-all { display: inline-flex; align-items: center; gap: 4px; }
	.limit-toggle-all[aria-expanded='false'] .limit-danger-caret { transform: rotate(-90deg); }
	.limit-danger-count { font-size: 11px; color: var(--md-sys-color-on-surface-variant); margin-left: auto; }
	.limit-label { flex: 1; min-width: 0; }
	.limit-label label { font-size: 13px; font-weight: 500; }
	.limit-hint { font-size: 11px; color: var(--md-sys-color-on-surface-variant); margin: 2px 0 0; }
	.danger-badge {
		display: inline-block; margin-left: 6px; padding: 1px 6px;
		border-radius: 999px; font-size: 10px; font-weight: 600;
		color: #fff; background: var(--md-sys-color-error, #ba1a1a);
		vertical-align: 1px;
	}
	.limit-input { display: flex; align-items: center; gap: 6px; }
	.limit-unit { font-size: 11px; color: var(--md-sys-color-on-surface-variant); min-width: 42px; }
	h1 { font-size: 24px; font-weight: 600; margin-bottom: var(--md-sys-space-xl); color: var(--md-sys-color-on-surface); }
	.section {
		background: var(--md-sys-color-surface-container);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-large);
		padding: var(--md-sys-space-lg); margin-bottom: var(--md-sys-space-lg);
	}
	.section h2 {
		font-size: 13px; font-weight: 600; color: var(--md-sys-color-on-surface-variant);
		text-transform: uppercase; letter-spacing: 1px; margin-bottom: var(--md-sys-space-lg);
	}
	.model-group-heading {
		font-size: 13px; font-weight: 600; color: var(--md-sys-color-on-surface-variant);
		margin-top: var(--md-sys-space-lg); margin-bottom: var(--md-sys-space-sm);
		text-transform: uppercase; letter-spacing: 0.5px;
	}
	.llm-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
		margin-bottom: var(--md-sys-space-sm);
	}
	.llm-head h2 { margin: 0; }
	.model-hint { font-size: 11px; color: var(--md-sys-color-on-surface-variant); margin-top: calc(-1 * var(--md-sys-space-sm)); margin-bottom: var(--md-sys-space-md); }
	.perm-list {
		margin-top: var(--md-sys-space-md);
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
	}
	.perm-list-title {
		font-size: 12px;
		font-weight: 700;
		color: var(--md-sys-color-on-surface);
		margin-bottom: var(--md-sys-space-xs);
	}
	.perm-row {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-extra-small);
		background: var(--md-sys-color-surface-container-low, rgba(0, 0, 0, 0.03));
	}
	.perm-key {
		flex: 1;
		font-size: 12px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.perm-effect {
		font-size: 11px;
		font-weight: 700;
		color: var(--md-sys-color-primary);
		text-transform: capitalize;
	}
	.perm-effect.deny { color: var(--md-sys-color-error); }
	.perm-revoke {
		border: none;
		background: transparent;
		color: var(--md-sys-color-on-surface-variant);
		font: inherit;
		font-size: 12px;
		cursor: pointer;
		padding: 2px 6px;
	}
	.perm-revoke:hover { color: var(--md-sys-color-error); }
	.shell-warning {
		margin-top: var(--md-sys-space-sm);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border: 1px solid var(--md-sys-color-error);
		border-radius: var(--md-sys-shape-medium);
		background: color-mix(in srgb, var(--md-sys-color-error) 10%, transparent);
		color: var(--md-sys-color-on-surface);
		font-size: 12px;
	}
	.shell-warning p { margin: 0 0 var(--md-sys-space-xs); }
	.shell-warning code {
		display: inline-block;
		padding: 2px 8px;
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-surface-container-high);
		font-family: var(--md-sys-typescale-mono);
		user-select: all;
	}
	.form-row {
		display: flex; align-items: center; margin-bottom: var(--md-sys-space-sm); gap: var(--md-sys-space-md);
	}
	.form-row label,
	.form-row .form-label { width: 120px; color: var(--md-sys-color-on-surface-variant); font-size: 13px; flex-shrink: 0; }

	.switch-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
	}
	.switch-label {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 13px;
	}

	.notify-grid-header, .notify-grid-row {
		display: grid;
		grid-template-columns: 1fr auto auto;
		gap: var(--md-sys-space-md);
		align-items: center;
		margin-bottom: var(--md-sys-space-sm);
	}
	.notify-grid-header {
		padding-bottom: var(--md-sys-space-xs);
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
		margin-bottom: var(--md-sys-space-md);
	}
	.notify-grid-header .switch-label {
		font-weight: 600;
		font-size: 11px;
		text-transform: uppercase;
	}

	.autostart-section .autostart-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
	}
	.autostart-label {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 13px;
	}
	.save-btn {
		position: sticky;
		bottom: var(--md-sys-space-xs);
		display: block;
		margin: var(--md-sys-space-md) auto 0;
		z-index: 1;
	}
	.recall-hint {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 13px;
	}
	.theme-toggle-row {
		display: flex;
		gap: var(--md-sys-space-sm);
		flex: 1;
	}
	.accent-picker {
		display: flex;
		gap: var(--md-sys-space-sm);
		flex: 1;
		flex-wrap: wrap;
	}
	.accent-swatch-selected {
		outline: 2px solid var(--md-sys-color-on-surface);
		outline-offset: -2px;
	}
	.custom-hex-input {
		width: 84px;
		font-family: var(--md-sys-typescale-mono);
		font-size: 14px;
		background: transparent;
		border: none;
		outline: none;
		color: inherit;
		padding: 0;
		text-align: center;
	}
	.custom-hex-input::placeholder {
		color: var(--md-sys-color-on-surface-variant);
		opacity: 0.6;
	}
	/* Log viewer dialog */
	:global(.md-dialog--wide) {
		width: min(760px, 92vw);
	}
	.log-path {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		margin: 0 0 var(--md-sys-space-sm);
		word-break: break-all;
	}
	.log-viewer {
		box-sizing: border-box;
		max-height: 60vh;
		overflow: auto;
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface);
		font-family: var(--md-sys-typescale-mono);
		font-size: 12px;
		line-height: 1.5;
		padding: var(--md-sys-space-md);
		border-radius: var(--md-sys-shape-small);
		border: 1px solid var(--md-sys-color-outline-variant);
		margin: 0;
		white-space: pre;
	}

	@media (max-width: 900px) {
		.limits-grid {
			grid-template-columns: 1fr;
		}
	}

	@media (max-width: 700px) {
		.limit-row,
		.form-row {
			flex-direction: column;
			align-items: stretch;
			gap: var(--md-sys-space-xs);
		}
		.form-row label,
		.form-row .form-label {
			width: auto;
			flex-shrink: 1;
		}
		.limit-input {
			justify-content: space-between;
		}
		.switch-row {
			align-items: flex-start;
		}
		.switch-label {
			padding-top: 6px;
		}
		.format-card {
			padding: var(--md-sys-space-sm);
		}
	}
</style>
