<script>
	import logger from '$lib/logger.js';
			import { onMount, onDestroy } from 'svelte';
	import { invoke } from '$lib/tauri.js';
	import { themeStore } from '$lib/themeStore.js';
	import MaterialSwitch from '$lib/MaterialSwitch.svelte';
	import MaterialDialog from '$lib/MaterialDialog.svelte';
	import MaterialNumberField from '$lib/MaterialNumberField.svelte';
	import MaterialSelect from '$lib/MaterialSelect.svelte';
	import MaterialAutocomplete from '$lib/MaterialAutocomplete.svelte';
	import StatusDot from '$lib/StatusDot.svelte';
	import HotkeyInput from '$lib/HotkeyInput.svelte';
	import { addNotification } from '$lib/stores.js';

	let llmConfig = $state({
		small_model: { provider: 'openai', api_style: '', model_name: 'gpt-4o-mini', temperature: 0, base_url: '', api_key: '', cost_per_1k_input_tokens: 0, cost_per_1k_output_tokens: 0, context_window: null },
		default_model: { provider: 'anthropic', api_style: '', model_name: 'claude-sonnet-4-20250514', temperature: 0.7, base_url: '', api_key: '', cost_per_1k_input_tokens: 0, cost_per_1k_output_tokens: 0, context_window: null },
		balanced_model: { provider: 'local', api_style: '', model_name: 'llama3', temperature: 0.7, base_url: 'http://localhost:11434', api_key: '', cost_per_1k_input_tokens: 0, cost_per_1k_output_tokens: 0, context_window: null },
		image_model: { provider: 'openai', api_style: '', model_name: 'gpt-4o', temperature: 0.2, base_url: '', api_key: '', cost_per_1k_input_tokens: 0, cost_per_1k_output_tokens: 0, context_window: null },
		audio_model: { provider: 'openai', api_style: '', model_name: 'gpt-4o-audio-preview', temperature: 0, base_url: '', api_key: '', cost_per_1k_input_tokens: 0, cost_per_1k_output_tokens: 0, context_window: null },
		embedding_model: { provider: 'openai', api_style: '', model_name: 'text-embedding-3-small', temperature: 0, base_url: '', api_key: '', cost_per_1k_input_tokens: 0, cost_per_1k_output_tokens: 0, context_window: null },
		// Named model library: reusable endpoint definitions roles reference.
		// Each entry: { name, endpoint: {...} }. role_models maps a role key
		// (e.g. `default_model`) to the library entry name it uses.
		models: [],
		role_models: {},
		stt_use_audio_model: true,
		vision_use_image_model: true,
		max_concurrent_requests: 2,
	});

	let keyConfigured = $state({
		small_model: false,
		default_model: false,
		balanced_model: false,
		image_model: false,
		audio_model: false,
		embedding_model: false,
		stt: false,
	});

	// Model library UI state: the popup manages named endpoints (`models`).
	let libraryOpen = $state(false);
	let editingIdx = $state(null); // index into llmConfig.models being edited; null = new
	let libraryForm = $state(null); // { name, endpoint: {...} }
	// Per-library-entry api_key configured status (from get_api_key_status).
	let keyConfiguredModels = $state({});
	// The six role keys; `audio_model` keeps its own inline card (STT handling).
	const ROLE_KEYS = ['default_model', 'balanced_model', 'small_model', 'image_model', 'embedding_model', 'audio_model'];

	// Single source of truth for the LLM endpoint cards; adding a model role
	// here renders its card without duplicating markup. Cards are grouped:
	// core models (agent loop) first, then specialized (vision / speech).
	const modelCards = [
		{ key: 'default_model', label: 'Default Model', hint: 'Primary reasoning & tool-use agent', prefix: 'dm', basePlaceholder: 'https://api.openai.com/v1', group: 'core' },
		{ key: 'balanced_model', label: 'Balanced Model', hint: 'Used when Default Model is unavailable', prefix: 'bm', basePlaceholder: 'http://localhost:11434', group: 'core' },
		{ key: 'small_model', label: 'Small Model', hint: 'Title generation & lightweight reasoning', prefix: 'sm', basePlaceholder: 'https://api.openai.com/v1', group: 'core' },
		{ key: 'image_model', label: 'Image Model', hint: 'Image understanding (vision-capable)', prefix: 'im', basePlaceholder: 'https://api.openai.com/v1', group: 'specialized' },
		{ key: 'audio_model', label: 'Audio Model', hint: 'Audio transcription (speech-to-text)', prefix: 'au', basePlaceholder: 'https://api.openai.com/v1', group: 'specialized' },
		{ key: 'embedding_model', label: 'Embedding Model', hint: 'Semantic memory: vectors for facts & past conversations. Local (Ollama / LM Studio) or cloud', prefix: 'em', basePlaceholder: 'https://api.openai.com/v1', group: 'specialized' },
	];
	const coreModelCards = modelCards.filter((c) => c.group === 'core');
	const specializedModelCards = modelCards.filter((c) => c.group === 'specialized');

	// Per-card model discovery: fetched model IDs from the provider's
	// `/models` endpoint, shown as autocomplete options on the Model field.
	// `stt` holds STT-provider model lists fetched with the STT key.
	let modelsByKey = $state({ stt: [], default_model: [], balanced_model: [], small_model: [], image_model: [], audio_model: [], embedding_model: [] });
	let modelFetching = $state({ stt: false, default_model: false, balanced_model: false, small_model: false, image_model: false, audio_model: false, embedding_model: false });
	let fetchTimers = {};
	// Timestamp of the last fetch notification per card, so bursts of
	// auto-fetch (focus / typing) don't spam the same message.
	let lastFetchNotify = $state({ stt: 0 });

	// Default base URL per API style, filled in when the user picks a style
	// and the Base URL field is still empty (never overwrites a custom URL).
	const styleDefaultBaseUrl = {
		'openai-chat': 'https://api.openai.com/v1',
		'openai-responses': 'https://api.openai.com/v1',
		'anthropic': 'https://api.anthropic.com',
		'gemini': 'https://generativelanguage.googleapis.com',
		'llama.cpp': 'http://127.0.0.1:8080',
	};

	const OPENAI_COMPAT_STT = new Set(['openai', 'groq']);
	const GEMINI_STT = new Set(['gemini']);

	// STT provider choices offered inside the Audio Model card's Provider
	// selector (values are prefixed `stt:` so they never collide with LLM
	// wire-protocol styles like `openai-chat`). The `openai-chat` LLM style
	// doubles as the "transcribe via the audio_model LLM" mode; Gemini audio
	// transcription is covered by the `stt:gemini` option, so no separate
	// LLM `gemini` style is offered for this slot.
	const STT_STYLE_OPTIONS = [
		{ value: 'stt:openai', label: 'OpenAI Whisper' },
		{ value: 'stt:groq', label: 'Groq' },
		{ value: 'stt:gemini', label: 'Google Gemini' },
		{ value: 'stt:deepgram', label: 'Deepgram' },
		{ value: 'stt:assemblyai', label: 'AssemblyAI' },
		{ value: 'stt:mcp', label: 'MCP Server' },
		{ value: 'stt:none', label: 'None' },
	];
	const STT_STYLE_SET = new Set(STT_STYLE_OPTIONS.map((o) => o.value));

	function sttProviderFromStyle(style) {
		return style.startsWith('stt:') ? style.slice(4) : null;
	}

	// Current value of the Audio Model card's API Style selector: either an LLM
	// wire-protocol style (`openai-chat` / …) or an `stt:*` provider.
	// Stored separately from `llmConfig.audio_model.api_style` so switching to
	// an STT provider never clobbers the endpoint's LLM wire protocol.
	let audioApiStyle = $state('openai-chat');

	function isOpenAiCompatibleStt(provider) {
		return OPENAI_COMPAT_STT.has(provider);
	}

	function isGeminiStt(provider) {
		return GEMINI_STT.has(provider);
	}

	// True when the Audio Model card's API Style is set to an STT provider.
	// In this mode the card's API Key / Model / Base URL fields bind to the
	// STT config instead of the audio_model endpoint, so credentials are
	// entered once and never duplicated in a separate STT section.
	function isAudioSttMode() {
		return STT_STYLE_SET.has(audioApiStyle);
	}

	function audioSttProvider() {
		return sttProviderFromStyle(audioApiStyle) || stt.provider;
	}

	function isCloudSttProvider(provider) {
		return ['openai', 'groq', 'gemini', 'deepgram', 'assemblyai'].includes(provider);
	}

	function sttModelPlaceholder(provider) {
		if (provider === 'deepgram') return 'nova-3';
		if (provider === 'assemblyai') return 'assemblyai_default';
		if (provider === 'groq') return 'whisper-large-v3-turbo';
		if (isGeminiStt(provider)) return 'gemini-2.5-flash';
		return 'whisper-1';
	}

	function sttBasePlaceholder(provider) {
		return isGeminiStt(provider) ? 'https://generativelanguage.googleapis.com/v1beta' : 'https://api.openai.com/v1';
	}

	// Model-list discovery for STT providers. OpenAI-compatible hosts expose
	// a `/models` endpoint (fetched like the LLM cards); Deepgram ships fixed
	// model ids; AssemblyAI has no public model list.
	function sttFetchBaseUrl(provider) {
		if (stt.base_url.trim()) return stt.base_url.trim();
		if (provider === 'groq') return 'https://api.groq.com/openai/v1';
		if (provider === 'gemini') return 'https://generativelanguage.googleapis.com/v1beta';
		return 'https://api.openai.com/v1';
	}

	function sttModelOptions(provider) {
		if (provider === 'deepgram') {
			return [
				{ value: 'nova-3', label: 'nova-3' },
				{ value: 'nova-2', label: 'nova-2' },
				{ value: 'whisper-large-v3', label: 'whisper-large-v3' },
				{ value: 'whisper-large-v3-turbo', label: 'whisper-large-v3-turbo' },
			];
		}
		if (provider === 'assemblyai') {
			return [
				{ value: 'assemblyai_default', label: 'AssemblyAI Default' },
				{ value: 'universal', label: 'universal' },
				{ value: 'universal-2', label: 'universal-2' },
				{ value: 'universal-3-pro', label: 'universal-3-pro' },
			];
		}
		return (modelsByKey.stt || []).map((m) => ({ value: m.id, label: m.name || m.id }));
	}

	async function fetchSttModels() {
		const provider = audioSttProvider();
		if (provider === 'deepgram' || provider === 'assemblyai' || provider === 'mcp' || provider === 'none') {
			return; // fixed lists or nothing to fetch
		}
		const base = sttFetchBaseUrl(provider);
		if (!base || (!stt.api_key && !keyConfigured.stt)) {
			modelsByKey.stt = [];
			return;
		}
		modelFetching.stt = true;
		try {
			const list = await invoke('discover_models', { baseUrl: base, apiKey: stt.api_key, role: 'stt' });
			const prev = JSON.stringify(modelsByKey.stt || []);
			modelsByKey.stt = list || [];
			if (JSON.stringify(modelsByKey.stt) !== prev) {
				notifyFetch('stt', `已获取 ${(list || []).length} 个模型`, 'success', 2500);
			}
		} catch (e) {
			const msg = typeof e === 'string' ? e : (e?.message || String(e));
			modelsByKey.stt = [];
			notifyFetch('stt', `获取模型失败: ${msg}`, 'error', 4000);
		} finally {
			modelFetching.stt = false;
		}
	}

	function scheduleSttFetch() {
		clearTimeout(fetchTimers.stt);
		fetchTimers.stt = setTimeout(() => fetchSttModels(), 600);
	}

	function onApiStyleChange(key, v) {
		if (key === 'audio_model') {
			if (STT_STYLE_SET.has(v)) {
				// STT provider picked: sync the STT config; the endpoint's LLM
				// api_style stays untouched so switching back to `openai-chat`
				// restores the previous wire protocol. Stale model-list caches
				// from the previous provider are dropped.
				audioApiStyle = v;
				const provider = sttProviderFromStyle(v);
				if (provider) stt.provider = provider;
				modelsByKey.stt = [];
				return;
			}
			// LLM style picked (openai-chat): transcription routes through
			// the audio_model endpoint, so there is no separate "LLM Adapter"
			// STT option. The endpoint provider is fixed to the wire protocol.
			stt.provider = 'llm';
			llmConfig.audio_model.provider = 'openai';
			audioApiStyle = v;
		}
		llmConfig[key].api_style = v;
		const url = styleDefaultBaseUrl[v];
		if (url && !llmConfig[key].base_url.trim()) {
			llmConfig[key].base_url = url;
		}
	}

	function notifyFetch(key, message, type, duration) {
		const now = Date.now();
		if (now - (lastFetchNotify[key] || 0) < 2500) return;
		lastFetchNotify[key] = now;
		addNotification(message, type, duration);
	}

	async function fetchModels(key) {
		const ep = llmConfig[key];
		// Stored keys are masked: when the slot is already configured the
		// backend falls back to the stored key via the role name.
		if (!ep || !ep.base_url || (!ep.api_key && !keyConfigured[key])) {
			modelsByKey[key] = [];
			return;
		}
		modelFetching[key] = true;
		try {
			const list = await invoke('discover_models', {
				baseUrl: ep.base_url,
				apiKey: ep.api_key,
				role: key,
			});
			const prev = modelsByKey[key] || [];
			const next = list || [];
			modelsByKey[key] = next;
			logger.debug('settings', `discovered ${next.length} models for ${key}`);
			if (JSON.stringify(prev) !== JSON.stringify(next)) {
				notifyFetch(key, `已获取 ${next.length} 个模型`, 'success', 2500);
			}
		} catch (e) {
			const msg = typeof e === 'string' ? e : (e?.message || String(e));
			modelsByKey[key] = [];
			notifyFetch(key, `获取模型失败: ${msg}`, 'error', 4000);
		} finally {
			modelFetching[key] = false;
		}
	}

	function scheduleFetch(key) {
		clearTimeout(fetchTimers[key]);
		fetchTimers[key] = setTimeout(() => fetchModels(key), 600);
	}

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

	let task = $state({ max_concurrent: 3, max_steps: 30 });
	let contextLimits = $state({
		compaction_ratio: 0.75,
		compaction_reserve_tokens: 4096,
		default_context_window: 128000,
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
		reminders_max: 32,
		reminders_due_horizon_secs: 365 * 24 * 3600,
		background_max_jobs: 64,
		event_chunk_batch_max_bytes: 8 * 1024,
		input_ring_buffer_secs: 20,
		embedding_chunk_size: 64,
	});

	// Data-driven limit editor. `danger: true` fields get a warning badge and
	// red styling: raising them widens the memory / cost / attack surface.
	const LIMIT_GROUPS = [
		{
			id: 'context',
			title: '上下文与压缩',
			hint: '模型上下文窗口与自动压缩（compaction）行为的阈值。压缩阈值过高可能导致上下文溢出。',
			fields: [
				{ key: 'default_context_window', label: '默认上下文窗口', unit: 'tokens', danger: true, hint: '端点未配置 context_window 且模型不在内置目录时的回退窗口。调高会增大每次请求的成本与溢出风险。' },
				{ key: 'compaction_ratio', label: '压缩触发比例', unit: '0–1', step: 0.01, min: 0.1, max: 0.95, danger: true, hint: '历史占用窗口的比例达到该值时开始压缩。调高 = 更晚压缩 = 更接近溢出。' },
				{ key: 'compaction_reserve_tokens', label: '压缩保留 token', unit: 'tokens', danger: false, hint: '计算压缩阈值时为模型回复预留的 token 数。' },
				{ key: 'max_observation_chars', label: '工具观察字符上限', unit: 'chars', danger: true, hint: '工具结果进入对话的最大字符数，也是 shell/file/process 等工具的默认输出截断上限（per-tool 可覆盖）。调大直接推高 token 成本。' },
				{ key: 'max_transcript_chars', label: '记忆提取转录上限', unit: 'chars', danger: true, hint: '事实提取时发送给模型的转录长度。' },
				{ key: 'notification_summary_chars', label: '通知摘要字符上限', unit: 'chars', danger: false },
				{ key: 'partial_checkpoint_min_chars', label: '流式检查点最小增量', unit: 'chars', danger: false, hint: '部分回复累计新增多少字符后落盘一次（崩溃恢复粒度）。' },
				{ key: 'partial_checkpoint_interval_secs', label: '流式检查点间隔', unit: 'secs', danger: false },
				{ key: 'fact_infer_interval_steps', label: '事实推断间隔', unit: 'steps', danger: false, hint: '长任务每多少步重新做一次事实提取。调小增加调用成本。' },
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
			id: 'resources',
			title: '资源上限',
			hint: '并发与内存资源保护。调大可能造成 CPU/内存/进程占用失控。',
			fields: [
				{ key: 'background_max_jobs', label: '后台作业并发上限', unit: 'count', danger: true, hint: '同时运行的 background shell 作业数。调大 = 子进程失控风险。' },
				{ key: 'reminders_max', label: '提醒数量上限', unit: 'count', danger: true },
				{ key: 'reminders_due_horizon_secs', label: '提醒最远排期', unit: 'days', days: true, danger: false },
				{ key: 'clipboard_history_entries', label: '剪贴板历史默认条数', unit: 'count', danger: false },
				{ key: 'clipboard_history_max_entries', label: '剪贴板历史上限', unit: 'count', danger: true },
				{ key: 'clipboard_entry_max_chars', label: '剪贴板条目截断', unit: 'chars', danger: false },
				{ key: 'event_chunk_batch_max_bytes', label: '事件分块批量上限', unit: 'KB', kb: true, danger: false, hint: 'agent 流式事件聚合分块的大小（IPC 频率与延迟权衡）。' },
				{ key: 'input_ring_buffer_secs', label: '音频环形缓冲', unit: 'secs', danger: true, hint: '录音缓冲时长。调大 = 内存增加 + 停止录音后仍会处理更长音频。' },
				{ key: 'embedding_chunk_size', label: '嵌入分块大小', unit: 'count', danger: false, hint: 'embedding 请求分块（提供方限制）。' },
			],
		},
	];

	function limitDisplay(key, value) {
		const f = LIMIT_GROUPS.flatMap((g) => g.fields).find((x) => x.key === key);
		if (!f) return value;
		if (f.mb) return Math.round((value / 1048576) * 10) / 10;
		if (f.kb) return Math.round((value / 1024) * 10) / 10;
		if (f.days) return Math.round((value / 86400) * 10) / 10;
		return value;
	}
	function limitCommit(key, v) {
		const f = LIMIT_GROUPS.flatMap((g) => g.fields).find((x) => x.key === key);
		if (!f) return v;
		if (f.mb) return Math.round(v * 1048576);
		if (f.kb) return Math.round(v * 1024);
		if (f.days) return Math.round(v * 86400);
		return v;
	}

	// Settings sub-tabs: general sections vs. the per-input-format handling
	// page. The full `context_limits` object is sent on save so fields the UI
	// does not render are never reset to defaults.
	let settingsTab = $state('general');
	const settingsTabs = [
		{ id: 'general', label: '常规' },
		{ id: 'input', label: '输入格式' },
		{ id: 'limits', label: '限制' },
	];
	let memory = $state({ session_window_size: 50, history_retention_days: 90 });
	let memoryRecall = $state({ query: '', kind: 'fact', results: [], loading: false });
	let memoryMaintenance = $state({ running: false, lastCount: null });
	let security = $state({ confirmation_mode: 'always', min_risk_level: 'medium' });

	let stt = $state({
		provider: 'mcp',
		mcp_server: '',
		api_key: '',
		model: '',
		base_url: '',
		timeout_secs: 30,
	});
	let notification = $state({
		task_created: { in_app: true, windows: false },
		task_completed: { in_app: true, windows: true },
		task_paused: { in_app: true, windows: false },
		task_resumed: { in_app: true, windows: false },
		task_error: { in_app: true, windows: true },
	});
	let log = $state({ level: 'info', file_enabled: true });

	// Log viewer (Logging section): reads the tail of the current log file
	// via get_log_info / read_log_tail and shows it in a dialog.
	let logView = $state({ open: false, path: '', content: '', loading: false });
	let logPreEl = $state(null);

	async function openLogViewer() {
		logView.loading = true;
		try {
			const info = await invoke('get_log_info');
			if (!info?.enabled) {
				addNotification('文件日志未启用，请先打开 File Logging', 'warning', 4000);
				return;
			}
			await refreshLogs();
			logView.open = true;
		} catch (e) {
			addNotification(e?.message || '无法读取日志', 'error', 4000);
		} finally {
			logView.loading = false;
		}
	}

	async function refreshLogs() {
		try {
			const data = await invoke('read_log_tail', { maxLines: 300 });
			logView.path = data.path;
			logView.content = data.content;
		} catch (e) {
			addNotification(e?.message || '无法读取日志', 'error', 4000);
		}
	}

	// Keep the viewer pinned to the newest lines whenever content changes.
	$effect(() => {
		if (logView.open && logPreEl) {
			logPreEl.scrollTop = logPreEl.scrollHeight;
		}
	});

	// Full facts management state (Memory section): every stored fact plus
	// the manual-add form. Backed by list_facts / add_fact / delete_fact.
	// Preferences live here too: they are facts tagged `preference` (single
	// memory channel), so there is no separate Preferences list.
	let facts = $state([]);
	let factsLoaded = $state(false);
	let newFact = $state({ predicate: '', object: '', tags: '' });
	let addingFact = $state(false);
	// Names of configured MCP servers, offered in the Audio Model card's
	// Model field when the STT provider is an MCP server.
	let mcpServerNames = $state([]);

	let keyChangeDialog = $state({ open: false, model: '', label: '' });
	let newKeyValue = $state('');
	let showKey = $state(false);
	let accent = $state(themeStore.currentAccent);
	let customAccentHex = $state(themeStore.isPreset ? '#2C5090' : themeStore.accentColor);
	let savedAccent = themeStore.currentAccent; // snapshot for reverting unsaved changes
	let currentTheme = $state(themeStore.currentTheme);
	const unsubTheme = themeStore.subscribe((v) => { currentTheme = v.theme; });
	// L12: guards against onDestroy running while onMount's async settings
	// load is still in flight.
	let mounted = true;

	onDestroy(() => {
		mounted = false;
		unsubTheme();
		if (accent !== savedAccent) {
			themeStore.setAccent(savedAccent);
		}
	});

	onMount(async () => {
		try {
			const settings = await invoke('get_settings');
			if (!mounted) return;
			if (settings) {
				llmConfig = settings.llm || llmConfig;
				// Seed the model library from the existing role endpoints on
				// first load (or when the persisted library is empty), so role
				// selection has entries to choose from. Also ensure every role
				// has a role_models entry.
				let seeded = false;
				if (!Array.isArray(llmConfig.models) || llmConfig.models.length === 0) {
					llmConfig.models = ROLE_KEYS.map((k) => ({
						name: k,
						endpoint: { ...llmConfig[k] },
					}));
					seeded = true;
				}
				llmConfig.role_models = llmConfig.role_models || {};
				for (const k of ROLE_KEYS) {
					if (seeded) {
						llmConfig.role_models[k] = k;
					} else if (!llmConfig.role_models[k]) {
						llmConfig.role_models[k] = '';
					}
				}
				hotkeyBinding = settings.hotkey?.key_binding || hotkeyBinding;
				hotkeyMode = settings.hotkey?.mode || 'toggle';
				audio = settings.audio || audio;
				task = settings.task || task;
				contextLimits = settings.context_limits || contextLimits;
				memory = settings.memory || memory;
				security = {
					confirmation_mode: settings.security?.confirmation_mode || 'always',
					min_risk_level: settings.security?.min_risk_level || 'medium',
				};
				stt = {
					provider: settings.stt?.provider || 'mcp',
					mcp_server: settings.stt?.mcp_server || '',
					api_key: settings.stt?.api_key || '',
					model: settings.stt?.model || '',
					base_url: settings.stt?.base_url || '',
					timeout_secs: settings.stt?.timeout_secs || 30,
				};
				// Audio Model Provider selector reflects the STT provider when
				// one is explicitly configured; `llm` maps back to the
				// endpoint's LLM wire style (the "transcribe via audio_model"
				// mode). A legacy `gemini` wire style is folded into the
				// `stt:gemini` option — Gemini audio transcription is a
				// single path now.
				const sttCfg = settings.stt;
				let llmAudioStyle = settings.llm?.audio_model?.api_style || '';
				if (llmAudioStyle === 'gemini') {
					audioApiStyle = 'stt:gemini';
					if (!sttCfg || sttCfg.provider === 'mcp') stt.provider = 'gemini';
				} else if (sttCfg?.provider === 'llm') {
					audioApiStyle = llmAudioStyle || 'openai-chat';
				} else if (sttCfg && (sttCfg.provider !== 'mcp' || sttCfg.mcp_server || sttCfg.api_key || sttCfg.model || sttCfg.base_url)) {
					audioApiStyle = `stt:${sttCfg.provider}`;
				} else {
					audioApiStyle = llmAudioStyle || 'openai-chat';
				}
				mcpServerNames = (settings.mcp_servers || []).map((s) => s.name || '').filter(Boolean);
			notification = settings.notification || notification;
			log = settings.log || log;
			if (settings.appearance?.accent_color) {
				accent = settings.appearance.accent_color;
				savedAccent = accent;
				themeStore.setAccent(accent);
				if (!themeStore.presets[accent]) customAccentHex = themeStore.accentColor;
			} else {
				savedAccent = themeStore.currentAccent;
			}
			}
		} catch (e) {
			addNotification(`加载设置失败: ${e}`, 'error', 4000);
		}
		try {
			const ks = await invoke('get_api_key_status');
			keyConfigured = ks;
			keyConfiguredModels = ks?.models || {};
			if (!mounted) return;
		} catch (e) {
			addNotification(`获取 API Key 状态失败: ${e}`, 'error', 3000);
		}
		try {
			autostartEnabled = await invoke('is_autostart_enabled');
			if (!mounted) return;
		} catch (e) {
			addNotification(`获取开机自启状态失败: ${e}`, 'error', 3000);
		}
		await loadFacts();
	});

	async function loadFacts() {
		try {
			facts = (await invoke('list_facts')) || [];
			factsLoaded = true;
		} catch {
			facts = [];
			logger.warn('settings', 'load facts error');
		}
	}

	async function addFact() {
		const predicate = newFact.predicate.trim();
		const object = newFact.object.trim();
		if (!predicate || !object) {
			addNotification('请输入 predicate 和 object', 'error', 3000);
			return;
		}
		addingFact = true;
		try {
			const tags = newFact.tags
				.split(',')
				.map((t) => t.trim())
				.filter(Boolean);
			const created = await invoke('add_fact', {
				subject: 'user',
				predicate,
				object,
				tags: tags.length ? tags : null,
			});
			facts = [created, ...facts];
			newFact = { predicate: '', object: '', tags: '' };
			addNotification('事实已保存', 'success', 2500);
		} catch (e) {
			addNotification(`添加事实失败: ${e}`, 'error', 3000);
		} finally {
			addingFact = false;
		}
	}

	async function deleteFact(factId) {
		try {
			await invoke('delete_fact', { factId });
			facts = facts.filter((f) => f.id !== factId);
		} catch (e) {
			addNotification(`删除事实失败: ${e}`, 'error', 3000);
		}
	}

	async function runRecall() {
		const q = memoryRecall.query.trim();
		if (!q) return;
		memoryRecall.loading = true;
		try {
			memoryRecall.results = (await invoke('recall_memory', {
				query: q,
				kind: memoryRecall.kind,
				limit: 10,
			})) || [];
		} catch (e) {
			memoryRecall.results = [];
			addNotification(`记忆检索失败: ${e}`, 'error', 4000);
		} finally {
			memoryRecall.loading = false;
		}
	}

	async function runMaintenance() {
		memoryMaintenance.running = true;
		try {
			memoryMaintenance.lastCount = await invoke('run_memory_maintenance');
			addNotification(`记忆维护完成（清理 ${memoryMaintenance.lastCount} 项）`, 'success', 3000);
		} catch (e) {
			addNotification(`记忆维护失败: ${e}`, 'error', 4000);
		} finally {
			memoryMaintenance.running = false;
		}
	}

	async function saveSettings() {
		try {
			await invoke('update_settings', {
				settings: {
					llm: llmConfig,
					hotkey: { key_binding: hotkeyBinding, mode: hotkeyMode, mute_hotkey: null },
					audio: {
						sample_rate: audio.sample_rate,
						channels: audio.channels,
						bits_per_sample: audio.bits_per_sample,
						max_duration_secs: audio.max_duration_secs,
						silence_timeout_ms: audio.silence_timeout_ms,
						vad_threshold: audio.vad_threshold,
					},
				task: {
					max_concurrent: task.max_concurrent,
					max_steps: task.max_steps,
				},
					memory: {
						session_window_size: memory.session_window_size,
						history_retention_days: memory.history_retention_days,
					},
				security: {
					confirmation_mode: security.confirmation_mode,
					min_risk_level: security.min_risk_level,
					encrypt_sensitive: true,
				},
				// Full object (loaded state kept intact) so fields the UI does
				// not render are preserved; backend applies it wholesale.
				context_limits: contextLimits,
					stt: {
						provider: stt.provider,
						mcp_server: stt.mcp_server || null,
						api_key: stt.api_key,
						model: stt.model,
						base_url: stt.base_url,
						timeout_secs: stt.timeout_secs,
					},
					notification: {
						task_created: { in_app: notification.task_created.in_app, windows: notification.task_created.windows },
						task_completed: { in_app: notification.task_completed.in_app, windows: notification.task_completed.windows },
						task_paused: { in_app: notification.task_paused.in_app, windows: notification.task_paused.windows },
						task_resumed: { in_app: notification.task_resumed.in_app, windows: notification.task_resumed.windows },
						task_error: { in_app: notification.task_error.in_app, windows: notification.task_error.windows },
					},
			log: {
				level: log.level,
				file_enabled: log.file_enabled,
				file_path: null,
			},
			appearance: {
				accent_color: accent,
			},
				},
			});
		addNotification('设置已保存', 'success');
			savedAccent = accent;
			if (autostartEnabled) {
				try { await invoke('enable_autostart'); } catch (e) {
					autostartEnabled = false;
					addNotification(`自动启动：${e}`, 'warning');
				}
			} else {
				try { await invoke('disable_autostart'); } catch (e) {
					autostartEnabled = true;
					addNotification(`取消自动启动：${e}`, 'warning');
				}
			}
		} catch (e) {
			addNotification(`保存设置失败: ${e}`, 'error', 5000);
		}
	}

	async function toggleAutostart() {
		autostartEnabled = !autostartEnabled;
	}

	function openKeyDialog(model, label) {
		keyChangeDialog = { open: true, model, label };
		newKeyValue = '';
		showKey = false;
	}

	function confirmKeyChange() {
		if (newKeyValue.trim()) {
			if (keyChangeDialog.model === 'stt') {
				stt.api_key = newKeyValue.trim();
			} else {
				llmConfig[keyChangeDialog.model].api_key = newKeyValue.trim();
				scheduleFetch(keyChangeDialog.model);
			}
			keyConfigured[keyChangeDialog.model] = true;
		}
		keyChangeDialog = { open: false, model: '', label: '' };
		newKeyValue = '';
		showKey = false;
	}

	function contrastText(hex) {
		const r = parseInt(hex.slice(1, 3), 16);
		const g = parseInt(hex.slice(3, 5), 16);
		const b = parseInt(hex.slice(5, 7), 16);
		const lum = (0.299 * r + 0.587 * g + 0.114 * b) / 255;
		return lum > 0.5 ? '#000000' : '#ffffff';
	}

	// --- Model library ------------------------------------------------------

	function libraryModelOptions() {
		return (llmConfig.models || []).map((m) => ({ value: m.name, label: m.name }));
	}

	// Role selection: pick a library entry (by name) and materialize its
	// endpoint into the role field. `''` means the role keeps a custom config.
	function selectRoleModel(roleKey, name) {
		llmConfig.role_models[roleKey] = name;
		const entry = (llmConfig.models || []).find((m) => m.name === name);
		if (!entry) return;
		llmConfig[roleKey] = { ...entry.endpoint };
		if (roleKey === 'audio_model') {
			// Sync the audio card's API-style state and STT routing from the
			// selected library entry.
			onApiStyleChange('audio_model', entry.endpoint.api_style || 'openai-chat');
		}
	}

	// The endpoint a role currently uses, either from its library selection or
	// its own inline config.
	function roleEndpoint(roleKey) {
		const name = llmConfig.role_models?.[roleKey];
		const entry = name ? (llmConfig.models || []).find((m) => m.name === name) : null;
		return entry ? entry.endpoint : llmConfig[roleKey];
	}

	function openLibrary() {
		libraryOpen = true;
		editingIdx = null;
		libraryForm = null;
	}

	function startAddModel() {
		editingIdx = null;
		libraryForm = {
			name: '',
			endpoint: {
				provider: 'openai',
				api_style: '',
				base_url: 'https://api.openai.com/v1',
				api_key: '',
				model_name: '',
				temperature: 0.7,
				context_window: null,
				cost_per_1k_input_tokens: 0,
				cost_per_1k_output_tokens: 0,
			},
		};
	}

	function startEditModel(idx) {
		const m = llmConfig.models[idx];
		editingIdx = idx;
		libraryForm = {
			name: m.name,
			endpoint: { ...m.endpoint, api_key: '' }, // api_key is masked; keep on save
		};
	}

	function deleteModel(idx) {
		const name = llmConfig.models[idx]?.name;
		if (!name) return;
		// Reset any role referencing the deleted entry to its own config.
		for (const k of ROLE_KEYS) {
			if (llmConfig.role_models[k] === name) llmConfig.role_models[k] = '';
		}
		llmConfig.models.splice(idx, 1);
		editingIdx = null;
		libraryForm = null;
	}

	function saveModel() {
		if (!libraryForm || !libraryForm.name.trim()) {
			addNotification('请填写模型名称', 'error', 3000);
			return;
		}
		const name = libraryForm.name.trim();
		const existing = llmConfig.models.findIndex((m) => m.name === name);
		if (editingIdx === null && existing !== -1) {
			addNotification('模型名称已存在', 'error', 3000);
			return;
		}
		// Preserve the previously stored api_key when the field was left empty.
		const prevKey = editingIdx !== null ? (llmConfig.models[editingIdx]?.endpoint?.api_key || '') : '';
		const endpoint = { ...libraryForm.endpoint, api_key: libraryForm.endpoint.api_key || prevKey };
		if (editingIdx === null) {
			llmConfig.models.push({ name, endpoint });
		} else {
			const oldName = llmConfig.models[editingIdx].name;
			llmConfig.models[editingIdx] = { name, endpoint };
			// Keep role references pointing at the renamed entry.
			if (oldName !== name) {
				for (const k of ROLE_KEYS) {
					if (llmConfig.role_models[k] === oldName) llmConfig.role_models[k] = name;
				}
			}
		}
		editingIdx = null;
		libraryForm = null;
		addNotification('模型已保存', 'success', 2000);
	}

	function modelApiStyleOptions(name, currentStyle) {
		if (name === 'audio_model' || (currentStyle || '').startsWith('stt:')) {
			return [
				{ value: 'openai-chat', label: 'OpenAI Chat Completions' },
				...STT_STYLE_OPTIONS,
			];
		}
		if (name === 'embedding_model') {
			return [
				{ value: 'openai-chat', label: 'OpenAI Chat Completions' },
				{ value: 'llama.cpp', label: 'llama.cpp server (local)' },
			];
		}
		return [
			{ value: 'openai-chat', label: 'OpenAI Chat Completions' },
			{ value: 'llama.cpp', label: 'llama.cpp server' },
			{ value: 'openai-responses', label: 'OpenAI Responses API' },
			{ value: 'anthropic', label: 'Anthropic (Claude)' },
			{ value: 'gemini', label: 'Google Gemini' },
		];
	}

	function isModelApiKeyConfigured(name) {
		return !!keyConfiguredModels[name];
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
			<HotkeyInput id="hotkey-binding" value={hotkeyBinding} onChange={(v) => { hotkeyBinding = v; }} />
		</div>
		<div class="form-row">
			<label for="hotkey-mode">Mode</label>
			<MaterialSelect id="hotkey-mode" value={hotkeyMode} options={[{ value: 'toggle', label: 'Toggle (press to start/stop)' }, { value: 'hold', label: 'Hold (push-to-talk)' }]} onChange={(v) => { hotkeyMode = v; }} />
		</div>
	</div>

	<div class="section">
		<h2>Audio</h2>
		<div class="form-row">
			<label for="audio-sample-rate">Sample Rate</label>
			<MaterialNumberField id="audio-sample-rate" value={audio.sample_rate} onChange={(v) => { audio.sample_rate = v; }} />
		</div>
		<div class="form-row">
			<label for="audio-channels">Channels</label>
			<MaterialNumberField id="audio-channels" value={audio.channels} min={1} max={2} onChange={(v) => { audio.channels = v; }} />
		</div>
		<div class="form-row">
			<label for="audio-max-duration">Max Duration (sec)</label>
			<MaterialNumberField id="audio-max-duration" value={audio.max_duration_secs} min={10} max={300} onChange={(v) => { audio.max_duration_secs = v; }} />
		</div>
		<div class="form-row">
			<label for="audio-silence-timeout">Silence Timeout (ms)</label>
			<MaterialNumberField id="audio-silence-timeout" value={audio.silence_timeout_ms} min={500} max={10000} step={100} onChange={(v) => { audio.silence_timeout_ms = v; }} />
		</div>
		<div class="form-row">
			<label for="audio-vad-threshold">VAD Threshold</label>
			<input id="audio-vad-threshold" type="range" class="md-slider" bind:value={audio.vad_threshold} min="0" max="1" step="0.05" style="--vad-fill: {audio.vad_threshold * 100}%" />
			<span class="range-value">{audio.vad_threshold}</span>
		</div>
	</div>

	<div class="section">
		<h2>STT (Speech-to-Text)</h2>
		<p class="model-hint">Provider 与全部配置（API Key / Model / Base URL / MCP Server）都在 Audio Model 行的 API Style 下拉框及其字段中完成。此处仅设置转写超时。</p>
		<div class="form-row">
			<label for="stt-timeout">Timeout (sec)</label>
			<MaterialNumberField id="stt-timeout" value={stt.timeout_secs} min={5} max={600} onChange={(v) => { stt.timeout_secs = v; }} />
		</div>
	</div>

	<div class="section">
		<h2>Task &amp; Concurrency</h2>
		<p class="model-hint">Max Concurrent 控制同时运行的任务数；LLM Per-Endpoint Concurrency 限制每个模型端点（角色）同时在途的请求数。后者低于前者时，超出上限的模型请求会排队等待，避免多个任务同时请求同一服务商触发限流（429）。</p>
		<div class="form-row">
			<label for="task-max-concurrent">Max Concurrent</label>
			<MaterialNumberField id="task-max-concurrent" value={task.max_concurrent} min={1} max={10} onChange={(v) => { task.max_concurrent = v; }} />
		</div>
		<div class="form-row">
			<label for="llm-max-concurrent-requests">LLM Per-Endpoint Concurrency</label>
			<MaterialNumberField id="llm-max-concurrent-requests" value={llmConfig.max_concurrent_requests} min={1} max={16} onChange={(v) => { llmConfig.max_concurrent_requests = v; }} />
		</div>
		<div class="form-row">
			<label for="task-max-steps">Max Steps</label>
			<MaterialNumberField id="task-max-steps" value={task.max_steps} min={1} max={100} onChange={(v) => { task.max_steps = v; }} />
		</div>
	</div>

	<div class="section">
		<h2>Memory</h2>
		<div class="form-row">
			<label for="memory-window-size">Window Size</label>
			<MaterialNumberField id="memory-window-size" value={memory.session_window_size} min={10} max={500} onChange={(v) => { memory.session_window_size = v; }} />
		</div>
		<div class="form-row">
			<label for="memory-retention">Retention (days)</label>
			<MaterialNumberField id="memory-retention" value={memory.history_retention_days} min={1} max={365} onChange={(v) => { memory.history_retention_days = v; }} />
		</div>
		<h3 class="model-group-heading">Recall &amp; Maintenance</h3>
		<p class="model-hint">检索已存储的记忆（事实 / 历史对话）。配置了 Embedding Model 时使用语义检索，否则回退到关键词匹配。维护会清理重复、敏感、过期的事实与残留向量。</p>
		<div class="form-row recall-row">
			<label for="memory-recall-query">Query</label>
			<input
				id="memory-recall-query"
				type="text"
				class="md-input"
				bind:value={memoryRecall.query}
				placeholder="e.g. dark theme"
				onkeydown={(e) => { if (e.key === 'Enter') runRecall(); }}
				autocomplete="off"
			/>
			<MaterialSelect
				id="memory-recall-kind"
				value={memoryRecall.kind}
				options={[
					{ value: 'fact', label: 'Facts' },
					{ value: 'episode', label: 'Conversations' },
				]}
				onChange={(v) => { memoryRecall.kind = v; }}
			/>
			<button class="md-btn" onclick={runRecall} disabled={memoryRecall.loading}>
				{memoryRecall.loading ? 'Searching…' : 'Search'}
			</button>
		</div>
		{#if memoryRecall.results.length > 0}
			<ul class="recall-results">
				{#each memoryRecall.results as r (r.entity_id + r.text)}
					<li>
						<span class="recall-score">{(r.score ?? 0).toFixed(2)}</span>
						<span class="recall-text">{r.text}</span>
					</li>
				{/each}
			</ul>
		{/if}
		<div class="form-row">
			<button class="md-btn" onclick={runMaintenance} disabled={memoryMaintenance.running}>
				{memoryMaintenance.running ? 'Running…' : 'Run Memory Maintenance'}
			</button>
			{#if memoryMaintenance.lastCount !== null}
				<span class="recall-hint">上次清理 {memoryMaintenance.lastCount} 项</span>
			{/if}
		</div>
		<h3 class="model-group-heading">Facts</h3>
		<p class="model-hint">Haven 记忆中的全部事实（身份、偏好、工作区等）。你可以手动添加、删除；agent 也会在你明确要求时用 facts 工具的 remember / forget 操作更新这里。</p>
		<div class="form-row add-fact-row">
			<input
				type="text"
				class="md-input"
				placeholder="predicate (e.g. email)"
				bind:value={newFact.predicate}
				autocomplete="off"
			/>
			<input
				type="text"
				class="md-input"
				placeholder="object (e.g. alice@example.com)"
				bind:value={newFact.object}
				autocomplete="off"
			/>
			<input
				type="text"
				class="md-input"
				placeholder="tags (optional, comma-separated)"
				bind:value={newFact.tags}
				autocomplete="off"
			/>
			<button class="md-btn" onclick={addFact} disabled={addingFact}>
				{addingFact ? 'Adding…' : 'Add Fact'}
			</button>
		</div>
		{#if factsLoaded && facts.length > 0}
			<div class="fact-list">
				{#each facts as fact}
					<div class="fact-row">
						<span class="fact-key">
							{#if fact.subject !== 'user'}{fact.subject}:{/if}{fact.predicate}
						</span>
						<span class="fact-value">
							{#if fact.source === 'inferred'}
								<span class="fact-tag fact-tag--inf">inferred</span>
							{:else}
								<span class="fact-tag fact-tag--user">user</span>
							{/if}
							{fact.object}
						</span>
						<button class="md-btn md-btn--xs md-btn--outlined" onclick={() => deleteFact(fact.id)} title="Delete fact">
							&times;
						</button>
					</div>
				{/each}
			</div>
		{:else if factsLoaded}
			<p class="model-hint">No facts recorded yet. They will appear here as you use Haven.</p>
		{/if}
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
					onclick={() => themeStore.setTheme('light')}
				>Light</button>
				<button
					class="md-btn"
					class:md-btn--outlined={currentTheme === 'dark'}
					class:md-btn--filled={currentTheme !== 'dark'}
					role="radio"
					aria-checked={currentTheme === 'dark'}
					onclick={() => themeStore.setTheme('dark')}
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
			<label for="security-min-level">Minimum Confirmation Level</label>
			<MaterialSelect id="security-min-level" value={security.min_risk_level} options={[
				{ value: 'safe', label: 'None (all auto-approved)' },
				{ value: 'low', label: 'Low & above' },
				{ value: 'medium', label: 'Medium & above' },
				{ value: 'high', label: 'High & above' },
				{ value: 'critical', label: 'Critical only' },
			]} onChange={(v) => { security.min_risk_level = v; }} />
		</div>
		<p class="model-hint">Operations at or above this risk level will require your confirmation. Low-level operations (file read, window list) will auto-approve.</p>
	</div>

	<div class="section notification-section">
		<h2>Notifications</h2>
		<div class="notify-grid-header">
			<span class="switch-label"></span>
			<span class="switch-label">In-App Toast</span>
			<span class="switch-label">Windows</span>
		</div>
		{#each [
			{ key: 'task_created', label: 'Task Start' },
			{ key: 'task_completed', label: 'Task Complete' },
			{ key: 'task_paused', label: 'Task Paused' },
			{ key: 'task_resumed', label: 'Task Resumed' },
			{ key: 'task_error', label: 'Task Error' },
		] as ev (ev.key)}
			<div class="notify-grid-row">
				<span class="switch-label">{ev.label}</span>
				<MaterialSwitch checked={notification[ev.key].in_app} onChange={(v) => { notification[ev.key].in_app = v; }} />
				<MaterialSwitch checked={notification[ev.key].windows} onChange={(v) => { notification[ev.key].windows = v; }} />
			</div>
		{/each}
	</div>

	<div class="section log-section">
		<div class="llm-head">
			<h2>Logging</h2>
			<button class="md-btn md-btn--outlined" onclick={openLogViewer} disabled={logView.loading}>查看日志</button>
		</div>
		<div class="form-row switch-row">
			<span class="switch-label">File Logging</span>
			<MaterialSwitch checked={log.file_enabled} onChange={(v) => { log.file_enabled = v; }} />
		</div>
		<div class="form-row">
			<label for="log-level">Log Level</label>
			<MaterialSelect id="log-level" value={log.level} options={[
				{ value: 'trace', label: 'Trace' },
				{ value: 'debug', label: 'Debug' },
				{ value: 'info', label: 'Info' },
				{ value: 'warn', label: 'Warn' },
				{ value: 'error', label: 'Error' },
			]} onChange={(v) => { log.level = v; }} />
		</div>
	</div>

	<div class="section autostart-section">
		<h2>Autostart</h2>
		<div class="form-row autostart-row">
			<span class="autostart-label">Launch Haven on system startup</span>
				<MaterialSwitch checked={autostartEnabled} onChange={toggleAutostart} />
		</div>
	</div>
	{/if}

	{#if settingsTab === 'input'}
	<div class="section">
		<div class="llm-head">
			<h2>LLM Configuration</h2>
			<button class="md-btn md-btn--outlined" onclick={openLibrary}>管理模型库</button>
		</div>
		<p class="model-hint">每个模型角色从「模型库」中选择一个模型使用。点击「管理模型库」可添加、编辑、删除不同种类的模型。录音转写与图片理解的模型路由开关在下方对应的输入格式卡片中配置。</p>

		{#snippet rolePicker(card)}
		<div class="model-card">
			<div class="picker-card">
				<div class="model-field model-role">
					<span class="field-label">{card.label}</span>
					<div class="role-hint">{card.hint}</div>
				</div>
				<div class="model-field">
					<span class="field-label">模型库</span>
					<MaterialSelect
						id="{card.prefix}-model-lib"
						value={llmConfig.role_models[card.key] || ''}
						options={libraryModelOptions()}
						onChange={(v) => selectRoleModel(card.key, v)}
					/>
				</div>
				<div class="model-field">
					<span class="field-label">当前模型</span>
					{#if llmConfig.role_models[card.key]}
						<span class="current-model">{roleEndpoint(card.key)?.model_name || '—'}</span>
					{:else}
						<span class="current-model custom">自定义: {llmConfig[card.key]?.model_name || '—'}</span>
					{/if}
				</div>
			</div>
			{#if card.key === 'audio_model'}
				{#if isAudioSttMode()}
					<div class="model-row audio-transport">
						<div class="model-field">
							<span class="field-label">STT 提供商</span>
							<span class="provider-note" title="Audio 行的 Provider 由 API Style 决定（LLM 或 STT 提供商）">LLM / STT</span>
						</div>
						<div class="model-field">
							<span class="field-label">Base URL</span>
							{#if isOpenAiCompatibleStt(audioSttProvider()) || isGeminiStt(audioSttProvider())}
								<input id="au-base-url" type="text" class="md-input" bind:value={stt.base_url} placeholder={sttBasePlaceholder(audioSttProvider())} autocomplete="off" />
							{:else}
								<span class="provider-note">由提供商默认</span>
							{/if}
						</div>
						<div class="model-field">
							<span class="field-label">Model</span>
							{#if audioSttProvider() === 'mcp'}
								<MaterialAutocomplete
									id="au-model"
									value={stt.mcp_server}
									options={mcpServerNames.map((n) => ({ value: n, label: n }))}
									placeholder="Pick a configured MCP server"
									loading={false}
									onChange={(v) => { stt.mcp_server = v; }}
								/>
							{:else if isCloudSttProvider(audioSttProvider())}
								<MaterialAutocomplete
									id="au-model"
									value={stt.model}
									options={sttModelOptions(audioSttProvider())}
									placeholder={sttModelPlaceholder(audioSttProvider())}
									loading={modelFetching.stt}
									onChange={(v) => { stt.model = v; }}
									onFocus={() => scheduleSttFetch()}
								/>
							{:else}
								<span class="provider-note">—</span>
							{/if}
						</div>
						<div class="model-field">
							<span class="field-label">API Key</span>
							<div class="key-cell" class:key-not-configured={!keyConfigured.stt}>
								<StatusDot color={keyConfigured.stt ? 'success' : 'outline'} />
								<button
									id="au-api-key"
									class="md-btn md-btn--xs md-btn--outlined"
									title={keyConfigured.stt ? 'Configured' : 'Not Configured'}
									onclick={() => openKeyDialog('stt', 'STT API Key')}
								>
									{keyConfigured.stt ? 'Change' : 'Set'}
								</button>
							</div>
						</div>
					</div>
				{:else}
					<div class="model-row">
						<div class="model-field">
							<span class="field-label">Temp</span>
							<MaterialNumberField id="au-temp" value={llmConfig.audio_model.temperature} step={0.1} min={0} max={2} onChange={(v) => { llmConfig.audio_model.temperature = v; }} />
						</div>
						<div class="model-field">
							<span class="field-label">Context</span>
							<MaterialNumberField id="au-context-window" value={llmConfig.audio_model.context_window ?? 0} step={1024} min={0} onChange={(v) => { llmConfig.audio_model.context_window = v > 0 ? Math.round(v) : null; }} />
						</div>
						<div class="model-field">
							<span class="field-label">Cost $/1K in / out</span>
							<div class="cost-cell">
								<MaterialNumberField id="au-cost-in" value={llmConfig.audio_model.cost_per_1k_input_tokens ?? 0} step={0.01} min={0} onChange={(v) => { llmConfig.audio_model.cost_per_1k_input_tokens = v; }} />
								<span class="cost-sep">/</span>
								<MaterialNumberField id="au-cost-out" value={llmConfig.audio_model.cost_per_1k_output_tokens ?? 0} step={0.01} min={0} onChange={(v) => { llmConfig.audio_model.cost_per_1k_output_tokens = v; }} />
							</div>
						</div>
					</div>
				{/if}
			{/if}
		</div>
		{/snippet}

		<div class="model-list">
			<div class="model-group">Core Models</div>
			{#each coreModelCards as card}
				{@render rolePicker(card)}
			{/each}
			<div class="model-group">Specialized Models</div>
			{#each specializedModelCards as card}
				{@render rolePicker(card)}
			{/each}
		</div>

		<p class="cost-hint">Leave empty to auto-detect. Context compaction triggers when estimated history reaches {Math.round(contextLimits.compaction_ratio * 100)}% of this (set in config.toml under [context_limits]).</p>
		<p class="cost-hint">USD per 1K tokens (input/output). Leave 0 to disable cost display for this model.</p>
	</div>

	{#if libraryOpen}
	<MaterialDialog open={true} title="模型库" onClose={() => { libraryOpen = false; libraryForm = null; }}>
		{#snippet children()}
			<div class="lib-list">
				{#each llmConfig.models as m, idx (m.name)}
					<div class="lib-item">
						<div class="lib-item-main">
							<span class="lib-name">{m.name}</span>
							<span class="lib-desc">{m.endpoint?.model_name || '—'} · {m.endpoint?.provider || '—'}</span>
							<span class="lib-key">
								<StatusDot color={isModelApiKeyConfigured(m.name) ? 'success' : 'outline'} />
								{isModelApiKeyConfigured(m.name) ? '已配置' : '未配置'}
							</span>
						</div>
						<div class="lib-item-actions">
							<button class="md-btn md-btn--xs md-btn--outlined" onclick={() => startEditModel(idx)}>编辑</button>
							<button class="md-btn md-btn--xs md-btn--outlined" onclick={() => deleteModel(idx)}>删除</button>
						</div>
					</div>
				{/each}
				{#if (llmConfig.models || []).length === 0}
					<p class="model-hint">模型库为空。点击「添加模型」创建第一个模型。</p>
				{/if}
			</div>
		{/snippet}
		{#snippet footer()}
			<button class="md-btn" onclick={startAddModel}>添加模型</button>
		{/snippet}
	</MaterialDialog>
	{/if}

	{#if libraryForm}
	<MaterialDialog open={true} title={editingIdx === null ? '添加模型' : '编辑模型'} onClose={() => { libraryForm = null; }}>
		{#snippet children()}
			<div class="lib-form">
				<div class="model-field">
					<span class="field-label">名称</span>
					<input type="text" class="md-input" bind:value={libraryForm.name} placeholder="唯一名称，角色据此选择" autocomplete="off" />
				</div>
				<div class="model-field">
					<span class="field-label">Provider</span>
					<input type="text" class="md-input" bind:value={libraryForm.endpoint.provider} autocomplete="off" />
				</div>
				<div class="model-field">
					<span class="field-label">API Style</span>
					<MaterialSelect
						id="lib-api-style"
						value={libraryForm.endpoint.api_style || 'openai-chat'}
						options={modelApiStyleOptions(libraryForm.name, libraryForm.endpoint.api_style)}
						onChange={(v) => { libraryForm.endpoint.api_style = v; }}
					/>
				</div>
				<div class="model-field">
					<span class="field-label">Base URL</span>
					<input type="text" class="md-input" bind:value={libraryForm.endpoint.base_url} autocomplete="off" />
				</div>
				<div class="model-field">
					<span class="field-label">Model</span>
					<input type="text" class="md-input" bind:value={libraryForm.endpoint.model_name} placeholder="模型标识" autocomplete="off" />
				</div>
				<div class="model-field">
					<span class="field-label">API Key</span>
					<input type="password" class="md-input" bind:value={libraryForm.endpoint.api_key} placeholder={isModelApiKeyConfigured(libraryForm.name) ? '已配置，留空保持不变' : ''} autocomplete="off" />
				</div>
				<div class="lib-form-row">
					<div class="model-field">
						<span class="field-label">Temp</span>
						<MaterialNumberField id="lib-temp" value={libraryForm.endpoint.temperature} step={0.1} min={0} max={2} onChange={(v) => { libraryForm.endpoint.temperature = v; }} />
					</div>
					<div class="model-field">
						<span class="field-label">Context</span>
						<MaterialNumberField id="lib-context" value={libraryForm.endpoint.context_window ?? 0} step={1024} min={0} onChange={(v) => { libraryForm.endpoint.context_window = v > 0 ? Math.round(v) : null; }} />
					</div>
				</div>
				<div class="lib-form-row">
					<div class="model-field">
						<span class="field-label">Cost $/1K in</span>
						<MaterialNumberField id="lib-cost-in" value={libraryForm.endpoint.cost_per_1k_input_tokens ?? 0} step={0.01} min={0} onChange={(v) => { libraryForm.endpoint.cost_per_1k_input_tokens = v; }} />
					</div>
					<div class="model-field">
						<span class="field-label">Cost $/1K out</span>
						<MaterialNumberField id="lib-cost-out" value={libraryForm.endpoint.cost_per_1k_output_tokens ?? 0} step={0.01} min={0} onChange={(v) => { libraryForm.endpoint.cost_per_1k_output_tokens = v; }} />
					</div>
				</div>
			</div>
		{/snippet}
		{#snippet footer()}
			<button class="md-btn" onclick={() => { libraryForm = null; }}>取消</button>
			<button class="md-btn md-btn--filled" onclick={saveModel}>保存</button>
		{/snippet}
	</MaterialDialog>
	{/if}


	<div class="section">
		<h2>输入格式</h2>
		<p class="model-hint">每种输入格式的处理方式与限制。保存后对聊天输入框生效，后端校验使用相同配置。</p>

		<div class="format-card">
			<h3>文本 Text</h3>
			<p class="model-hint">文字指令直接发送给 Default Model 处理。语音转写结果也以文本形式进入同一通道，无需额外配置。</p>
		</div>

		<div class="format-card">
			<h3>图片 Image</h3>
			<p class="model-hint">
				粘贴或选取的图片先压缩为 JPEG（最长边 ≤{contextLimits.max_attachment_image_dim_px}px、质量
				{Math.round(contextLimits.attachment_image_jpeg_quality * 100)}%），再交由视觉模型理解；关闭专用模型后改由
				Default Model 处理。
			</p>
			<div class="form-row switch-row">
				<span class="switch-label">图片理解使用专用视觉模型</span>
				<MaterialSwitch checked={llmConfig.vision_use_image_model} onChange={(v) => { llmConfig.vision_use_image_model = v; }} />
			</div>
			<div class="form-row">
				<label for="max-attachment-images">单条消息最多图片数</label>
				<MaterialNumberField
					id="max-attachment-images"
					value={contextLimits.max_attachment_images}
					min={1}
					max={20}
					step={1}
					onChange={(v) => { contextLimits.max_attachment_images = v; }}
				/>
			</div>
			<div class="form-row">
				<label for="max-attachment-image-mb">单张图片大小上限 (MiB)</label>
				<MaterialNumberField
					id="max-attachment-image-mb"
					value={Math.round((contextLimits.max_attachment_image_bytes / 1048576) * 10) / 10}
					min={1}
					max={50}
					step={1}
					onChange={(v) => { contextLimits.max_attachment_image_bytes = Math.round(v * 1024 * 1024); }}
				/>
			</div>
			<div class="form-row">
				<label for="max-attachment-image-dim">压缩最长边 (px)</label>
				<MaterialNumberField
					id="max-attachment-image-dim"
					value={contextLimits.max_attachment_image_dim_px}
					min={512}
					max={4096}
					step={64}
					onChange={(v) => { contextLimits.max_attachment_image_dim_px = v; }}
				/>
			</div>
			<div class="form-row">
				<label for="attachment-image-quality">JPEG 压缩质量</label>
				<MaterialNumberField
					id="attachment-image-quality"
					value={contextLimits.attachment_image_jpeg_quality}
					min={0.1}
					max={1}
					step={0.05}
					onChange={(v) => { contextLimits.attachment_image_jpeg_quality = v; }}
				/>
			</div>
		</div>

		<div class="format-card">
			<h3>文件 File</h3>
			<p class="model-hint">
				附件以 base64 上传，后端保存到磁盘，agent 通过 file 工具读取路径进行处理，无需额外配置。
			</p>
			<div class="form-row">
				<label for="max-attachment-files">单条消息最多文件数</label>
				<MaterialNumberField
					id="max-attachment-files"
					value={contextLimits.max_attachment_files}
					min={1}
					max={20}
					step={1}
					onChange={(v) => { contextLimits.max_attachment_files = v; }}
				/>
			</div>
			<div class="form-row">
				<label for="max-attachment-file-mb">单个文件大小上限 (MiB)</label>
				<MaterialNumberField
					id="max-attachment-file-mb"
					value={Math.round((contextLimits.max_attachment_file_bytes / 1048576) * 10) / 10}
					min={1}
					max={100}
					step={1}
					onChange={(v) => { contextLimits.max_attachment_file_bytes = Math.round(v * 1024 * 1024); }}
				/>
			</div>
		</div>

		<div class="format-card">
			<h3>语音 Voice</h3>
			<p class="model-hint">
				按住热键录音，经 STT 转写为文本后作为普通消息发送；转写可走专用音频模型或直接使用 Default Model。
			</p>
			<div class="form-row switch-row">
				<span class="switch-label">录音转写使用专用音频模型</span>
				<MaterialSwitch checked={llmConfig.stt_use_audio_model} onChange={(v) => { llmConfig.stt_use_audio_model = v; }} />
			</div>
			<p class="model-hint">STT 提供商与录音参数（VAD、采样率、时长上限）在「常规 → Audio / STT」中配置。</p>
		</div>
	</div>
	{/if}

	{#if settingsTab === 'limits'}
	<div class="limits-grid">
		{#each LIMIT_GROUPS as group}
		<div class="format-card limit-card">
			<h3>{group.title}</h3>
			<p class="model-hint">{group.hint}</p>
			{#each group.fields as f}
			<div class="form-row limit-row" class:danger-row={f.danger}>
				<div class="limit-label">
					<label for="limit-{f.key}">{f.label}</label>
					{#if f.danger}
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
						onChange={(v) => { contextLimits[f.key] = limitCommit(f.key, v); }}
					/>
					<span class="limit-unit">{f.unit}</span>
				</div>
			</div>
			{/each}
		</div>
		{/each}
	</div>
	{/if}

	<button class="md-btn md-btn--filled save-btn" onclick={saveSettings}>
		Save Settings
	</button>
</div>

<MaterialDialog
	open={keyChangeDialog.open}
	onClose={() => { keyChangeDialog = { open: false, model: '', label: '' }; newKeyValue = ''; showKey = false; }}
	title={keyChangeDialog.model ? (keyConfigured[keyChangeDialog.model] ? 'Change API Key' : 'Set API Key') : 'API Key'}
>
	{#snippet children()}
		<p class="dialog-hint">Enter the API key for <strong>{keyChangeDialog.label}</strong>.</p>
		<div class="key-input-row">
			<input
				type={showKey ? 'text' : 'password'}
				class="md-input"
				bind:value={newKeyValue}
				placeholder="sk-..."
				autocomplete="new-password"
			/>
			<button
				class="key-visibility-btn"
				type="button"
				aria-label={showKey ? 'Hide API key' : 'Show API key'}
				title={showKey ? 'Hide API key' : 'Show API key'}
				onclick={() => { showKey = !showKey; }}
			>
				{#if showKey}
					<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19m-6.72-1.07a3 3 0 1 1-4.24-4.24" /><line x1="1" y1="1" x2="23" y2="23" /></svg>
				{:else}
					<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z" /><circle cx="12" cy="12" r="3" /></svg>
				{/if}
			</button>
		</div>
	{/snippet}
	{#snippet footer()}
		<button
			class="md-btn md-btn--text"
			onclick={() => { keyChangeDialog = { open: false, model: '', label: '' }; newKeyValue = ''; showKey = false; }}
		>
			Cancel
		</button>
		<button class="md-btn md-btn--filled" onclick={confirmKeyChange}>Confirm</button>
	{/snippet}
</MaterialDialog>

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
	.limit-row { display: flex; justify-content: space-between; align-items: flex-start; gap: var(--md-sys-space-md); }
	.limit-row.danger-row {
		border: 1px solid var(--md-sys-color-error, #ba1a1a);
		border-radius: var(--md-sys-shape-small);
		padding: 6px 8px;
		margin-bottom: 6px;
		background: color-mix(in srgb, var(--md-sys-color-error, #ba1a1a) 6%, transparent);
	}
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
	.model-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-md);
	}
	.model-group {
		font-size: 11px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 1px;
		color: var(--md-sys-color-primary);
		margin: var(--md-sys-space-md) 0 var(--md-sys-space-xs);
	}
	.model-group:first-child { margin-top: 0; }
	.llm-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
		margin-bottom: var(--md-sys-space-sm);
	}
	.llm-head h2 { margin: 0; }
	.picker-card {
		display: grid;
		grid-template-columns: minmax(200px, 1.2fr) minmax(180px, 1fr) minmax(160px, 1fr);
		gap: var(--md-sys-space-lg);
		align-items: end;
	}
	.current-model {
		font-size: 13px;
		color: var(--md-sys-color-primary);
		font-weight: 500;
		word-break: break-word;
	}
	.current-model.custom {
		color: var(--md-sys-color-on-surface-variant);
		font-weight: 400;
	}
	.lib-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-sm);
		max-height: 40vh;
		overflow-y: auto;
	}
	.lib-item {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-small);
		background: var(--md-sys-color-surface-container-low);
	}
	.lib-item-main {
		display: flex;
		flex-direction: column;
		gap: 2px;
		min-width: 0;
	}
	.lib-name {
		font-size: 13px;
		font-weight: 600;
		color: var(--md-sys-color-on-surface);
	}
	.lib-desc {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.lib-key {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
	}
	.lib-item-actions {
		display: flex;
		gap: var(--md-sys-space-xs);
		flex-shrink: 0;
	}
	.lib-form {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-md);
	}
	.lib-form-row {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: var(--md-sys-space-md);
	}
	.model-card {
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		background: var(--md-sys-color-surface-container-lowest);
		padding: var(--md-sys-space-md);
	}
	.model-row {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(170px, 1fr));
		gap: var(--md-sys-space-md);
		align-items: end;
	}
	.audio-transport {
		margin-top: var(--md-sys-space-md);
		padding-top: var(--md-sys-space-md);
		border-top: 1px dashed var(--md-sys-color-outline-variant);
	}
	.model-field {
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 4px;
	}
	.model-field .md-input { width: 100%; }
	.model-field :global(.md-number-field) { width: 100%; }
	.model-field :global(.md-select-container),
	.model-field :global(.ma-root) { width: 100%; }
	.field-label {
		font-size: 11px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.5px;
		color: var(--md-sys-color-on-surface-variant);
		white-space: nowrap;
	}
	.model-role .field-label {
		color: var(--md-sys-color-primary);
		font-size: 13px;
	}
	.role-hint { font-size: 11px; color: var(--md-sys-color-on-surface-variant); margin-top: 2px; line-height: 1.4; }
	.provider-note { font-size: 11px; color: var(--md-sys-color-on-surface-variant); font-style: italic; }
	.key-cell {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		min-height: var(--md-comp-textfield-container-height);
	}
	.key-cell .md-btn { flex-shrink: 0; min-width: 64px; }
	.cost-cell { display: flex; align-items: center; gap: 4px; }
	.cost-cell :global(.md-number-field) { width: 74px; flex-shrink: 0; }
	.key-input-row { display: flex; align-items: center; gap: var(--md-sys-space-xs); }
	.key-input-row .md-input { flex: 1; min-width: 0; }
	.key-visibility-btn {
		background: none;
		border: 1px solid var(--md-sys-color-outline-variant);
		color: var(--md-sys-color-on-surface-variant);
		width: 38px;
		height: 38px;
		border-radius: var(--md-sys-shape-small);
		display: inline-flex;
		align-items: center;
		justify-content: center;
		cursor: pointer;
		flex-shrink: 0;
		transition: background-color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard),
			color var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}
	.key-visibility-btn:hover {
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface);
	}
	.model-hint { font-size: 11px; color: var(--md-sys-color-on-surface-variant); margin-top: calc(-1 * var(--md-sys-space-sm)); margin-bottom: var(--md-sys-space-md); }
	.form-row {
		display: flex; align-items: center; margin-bottom: var(--md-sys-space-sm); gap: var(--md-sys-space-md);
	}
	.form-row label,
	.form-row .form-label { width: 120px; color: var(--md-sys-color-on-surface-variant); font-size: 13px; flex-shrink: 0; }

	.cost-sep {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 12px;
		flex-shrink: 0;
	}
	.cost-hint {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		margin-top: calc(-1 * var(--md-sys-space-xs));
		margin-bottom: var(--md-sys-space-md);
	}

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

	.form-row input[type='range'] { flex: 1; }
	.md-slider {
		-webkit-appearance: none;
		appearance: none;
		width: 100%;
		height: 4px;
		outline: none;
		cursor: pointer;
		flex: 1;
		margin: 18px 0;
		padding: 0;
		background: transparent;
	}
	.md-slider::-webkit-slider-runnable-track {
		height: 4px;
		border-radius: 2px;
		background: linear-gradient(to right, var(--md-sys-color-primary) var(--vad-fill, 50%), var(--md-sys-color-surface-container-highest) var(--vad-fill, 50%));
	}
	.md-slider::-webkit-slider-thumb {
		-webkit-appearance: none;
		appearance: none;
		width: 16px;
		height: 16px;
		border-radius: 50%;
		background: var(--md-sys-color-primary);
		cursor: pointer;
		box-shadow: var(--md-sys-elevation-1);
		margin-top: -6px;
		transition: transform var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-standard);
	}
	.md-slider::-webkit-slider-thumb:hover {
		transform: scale(1.25);
	}
	.md-slider::-moz-range-track {
		height: 4px;
		border-radius: 2px;
		background: var(--md-sys-color-surface-container-highest);
		border: none;
	}
	.md-slider::-moz-range-thumb {
		width: 16px;
		height: 16px;
		border-radius: 50%;
		background: var(--md-sys-color-primary);
		border: none;
		cursor: pointer;
		box-shadow: var(--md-sys-elevation-1);
	}
	.md-slider::-moz-range-thumb:hover {
		transform: scale(1.25);
	}
	.md-slider:focus-visible::-webkit-slider-thumb {
		box-shadow: 0 0 0 4px color-mix(in srgb, var(--md-sys-color-primary) 20%, transparent);
	}
	.md-slider:focus-visible::-moz-range-thumb {
		box-shadow: 0 0 0 4px color-mix(in srgb, var(--md-sys-color-primary) 20%, transparent);
	}
	.range-value {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		height: 40px;
		min-width: 44px;
		padding: 0 var(--md-sys-space-sm);
		color: var(--md-sys-color-on-surface-variant);
		font-size: 14px;
		font-weight: 500;
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
	.recall-row {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		flex-wrap: wrap;
	}
	.recall-row .md-input {
		flex: 1;
		min-width: 200px;
	}
	.recall-results {
		list-style: none;
		margin: var(--md-sys-space-sm) 0 var(--md-sys-space-md);
		padding: 0;
		max-height: 220px;
		overflow-y: auto;
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-radius-md);
	}
	.recall-results li {
		display: flex;
		align-items: baseline;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		border-bottom: 1px solid var(--md-sys-color-outline-variant);
		font-size: 13px;
	}
	.recall-results li:last-child {
		border-bottom: none;
	}
	.recall-score {
		font-variant-numeric: tabular-nums;
		color: var(--md-sys-color-primary);
		min-width: 42px;
	}
	.recall-text {
		color: var(--md-sys-color-on-surface);
		overflow-wrap: anywhere;
	}
	.recall-hint {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 13px;
	}
	.dialog-hint {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 14px;
		line-height: 1.5;
		margin-bottom: var(--md-sys-space-lg);
	}
	.fact-list {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
	}
	.fact-row {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-xs) 0;
	}
	.fact-key {
		color: var(--md-sys-color-on-surface-variant);
		font-size: 13px;
		font-weight: 500;
		min-width: 140px;
		flex-shrink: 0;
	}
	.fact-value {
		color: var(--md-sys-color-on-surface);
		font-size: 13px;
		flex: 1;
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
	}
	.fact-tag {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		height: 20px;
		padding: 0 6px;
		border-radius: var(--md-sys-shape-small);
		font-size: 10px;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.5px;
		flex-shrink: 0;
	}
	.fact-tag--user {
		background: var(--md-sys-color-primary-container);
		color: var(--md-sys-color-on-primary-container);
	}
	.fact-tag--inf {
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
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
</style>
