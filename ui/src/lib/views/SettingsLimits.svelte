<script lang="ts">
	import MaterialCollapsible from '$lib/MaterialCollapsible.svelte';
	import MaterialButton from '$lib/MaterialButton.svelte';
	import MaterialBadge from '$lib/MaterialBadge.svelte';
	import Icon from '$lib/Icon.svelte';
	import MaterialCard from '$lib/MaterialCard.svelte';
	import MaterialNumberFieldWithUnit from '$lib/MaterialNumberFieldWithUnit.svelte';
	import { withNumberValue } from '$lib/typedCallbacks.ts';
	import {
		CONCURRENCY_BOUNDS,
		CONTEXT_WINDOW_BOUNDS,
		RESOURCE_LIMIT_BOUNDS,
	} from '$lib/contracts/settings.ts';
	import {
		MAX_FACTS_IN_EXTRACTION_PROMPT,
		type ContextLimitsConfigInput,
	} from '$lib/contracts/generatedCommands.ts';

	type LimitKey = keyof ContextLimitsConfigInput;
	interface LimitField {
		key: LimitKey;
		label: string;
		unit: string;
		kTokens?: boolean;
		mb?: boolean;
		kb?: boolean;
		days?: boolean;
		step?: number;
		displayDigits?: number;
		min?: number;
		max?: number;
		danger: boolean;
		hint?: string;
	}
	interface LimitGroup {
		id: string;
		title: string;
		hint: string;
		fields: LimitField[];
	}
	interface Props {
		contextLimits: Partial<ContextLimitsConfigInput>;
		modelContextWindows?: Array<number | null | undefined>;
	}

	let { contextLimits, modelContextWindows = [] }: Props = $props();
	const MAX_CONTEXT_WINDOW_K_TOKENS = CONTEXT_WINDOW_BOUNDS.maxTokens / 1000;

	const LIMIT_GROUPS: LimitGroup[] = [
		{
			id: 'context',
			title: '上下文与压缩',
			hint: '模型上下文窗口与自动压缩（compaction）行为的阈值。压缩阈值过高可能导致上下文溢出。',
			fields: [
				{
					key: 'default_context_window',
					label: '默认上下文窗口',
					unit: 'K',
					kTokens: true,
					step: 1,
					min: CONTEXT_WINDOW_BOUNDS.minTokens / 1000,
					max: MAX_CONTEXT_WINDOW_K_TOKENS,
					danger: true,
					hint: '角色未填写 Context 且 Provider /models 未返回上下文长度时的回退窗口（单位 K = 1000 tokens，如 64 = 64K）。范围 1K–2,000K；调高会增大请求成本与溢出风险。',
				},
				{
					key: 'response_token_floor',
					label: '回复输出 token 下限',
					unit: 'tokens',
					max: CONTEXT_WINDOW_BOUNDS.maxTokens,
					danger: false,
					hint: '每个模型端点的 max_tokens 会被抬到不低于此值（取两者较大），最多 2,000,000 tokens。默认 32000。',
				},
				{
					key: 'compaction_ratio',
					label: '压缩触发比例',
					unit: '0–1',
					step: 0.01,
					min: 0.1,
					max: 0.95,
					danger: true,
					hint: '历史占用窗口的比例达到该值时开始压缩。调高 = 更晚压缩 = 更接近溢出。',
				},
				{
					key: 'compaction_reserve_tokens',
					label: '压缩保留 token',
					unit: 'tokens',
					max: CONTEXT_WINDOW_BOUNDS.maxTokens - 1,
					danger: false,
					hint: '计算压缩阈值时为模型回复预留的 token 数；必须小于默认窗口和每个显式模型窗口。',
				},
				{
					key: 'max_observation_chars',
					label: '工具观察字符上限',
					unit: 'chars',
					max: RESOURCE_LIMIT_BOUNDS.contextTextChars.max,
					danger: true,
					hint: '工具结果进入对话的最大字符数，也是 shell/files 等工具的默认输出截断上限（per-tool 只能调低）。上限 100,000；调大推高 token 成本。',
				},
				{
					key: 'max_tools_per_request',
					label: '单次请求工具数上限',
					unit: 'count',
					min: RESOURCE_LIMIT_BOUNDS.toolsPerRequest.min,
					max: RESOURCE_LIMIT_BOUNDS.toolsPerRequest.max,
					danger: true,
					hint: '发给模型的 tools 数组最大长度。范围 1–350；超过上限可能被提供方拒绝。',
				},
				{
					key: 'max_transcript_chars',
					label: '记忆提取转录上限',
					unit: 'chars',
					max: RESOURCE_LIMIT_BOUNDS.contextTextChars.max,
					danger: true,
					hint: '事实提取时发送给模型的转录长度，最多 100,000 字符。',
				},
				{
					key: 'tool_run_result_context_chars',
					label: '任务结果进入对话的上限',
					unit: 'chars',
					max: RESOURCE_LIMIT_BOUNDS.contextTextChars.max,
					danger: true,
					hint: '后台任务完成后放入所属会话的结果字符数；完整输出仍保存在日志中，最多 100,000 字符。',
				},
				{
					key: 'notification_summary_chars',
					label: '通知摘要字符上限',
					unit: 'chars',
					max: RESOURCE_LIMIT_BOUNDS.notificationSummaryChars.max,
					danger: false,
				},
				{
					key: 'partial_checkpoint_min_chars',
					label: '流式检查点最小增量',
					unit: 'chars',
					danger: false,
					hint: '部分回复累计新增多少字符后落盘一次（崩溃恢复粒度）。',
				},
				{
					key: 'partial_checkpoint_interval_secs',
					label: '流式检查点间隔',
					unit: 'secs',
					danger: false,
				},
				{
					key: 'fact_infer_interval_steps',
					label: '事实推断间隔',
					unit: 'steps',
					danger: false,
					hint: '长会话每多少步重新做一次事实提取；设为 0 可关闭步数触发。调小会增加调用成本。',
				},
				{
					key: 'fact_extraction_min_interval_secs',
					label: '事实提取最短间隔',
					unit: 'secs',
					danger: false,
					hint: '限制短时间内重复调用模型提取事实的频率；设为 0 可关闭时间节流。',
				},
				{
					key: 'max_known_facts',
					label: '提示中已知事实数',
					unit: 'count',
					max: MAX_FACTS_IN_EXTRACTION_PROMPT,
					danger: false,
					hint: `默认值 40；上限 ${MAX_FACTS_IN_EXTRACTION_PROMPT}，避免事实上下文过大。`,
				},
				{
					key: 'sanitize_field_max_chars',
					label: '事实字段消毒长度',
					unit: 'chars',
					min: RESOURCE_LIMIT_BOUNDS.sanitizeFieldChars.min,
					max: RESOURCE_LIMIT_BOUNDS.sanitizeFieldChars.max,
					danger: true,
					hint: '事实字段消毒的统一截断长度，范围 1–256 字符；系统提示构建也按同一 256 字符硬上限处理。',
				},
			],
		},
		{
			id: 'files',
			title: '文件与搜索工具',
			hint: 'files 工具读取、写入、总结与搜索的资源上限。',
			fields: [
				{
					key: 'file_max_patch_bytes',
					label: '定向 patch 文件上限',
					unit: 'MiB',
					mb: true,
					min: RESOURCE_LIMIT_BOUNDS.filePatchBytes.min / 1048576,
					max: RESOURCE_LIMIT_BOUNDS.filePatchBytes.max / 1048576,
					danger: true,
					hint: 'files.patch 可定向载入的文件上限，独立于完整 read 上限；默认 8 MiB，最多 64 MiB。',
				},
				{
					key: 'file_max_full_file_bytes',
					label: '整文件读取上限',
					unit: 'bytes',
					max: RESOURCE_LIMIT_BOUNDS.fileOperationBytes.max,
					danger: true,
					hint: 'files.read 整体载入文件时的字节上限，也作为未指定 limit 时的默认分段跨度；patch 有独立上限。最多 64 MiB。',
				},
				{
					key: 'file_byte_read_max_bytes',
					label: '字节读取与哈希上限',
					unit: 'MiB',
					mb: true,
					min: RESOURCE_LIMIT_BOUNDS.fileByteReadBytes.min / 1048576,
					max: RESOURCE_LIMIT_BOUNDS.fileByteReadBytes.max / 1048576,
					danger: true,
					hint: 'byte 模式单次读取上限至少 1 字节、最多 64 MiB；inspect/hash 仍有独立 64 MiB 硬上限。',
				},
				{
					key: 'file_write_max_bytes',
					label: '单次写入上限',
					unit: 'MiB',
					mb: true,
					min: RESOURCE_LIMIT_BOUNDS.fileWriteBytes.min / 1048576,
					max: RESOURCE_LIMIT_BOUNDS.fileWriteBytes.max / 1048576,
					danger: true,
					hint: 'write、edit、patch 单次产出文件的字节上限。',
				},
				{ key: 'file_line_span', label: '行模式默认跨度', unit: 'lines', danger: false },
				{
					key: 'file_line_max_bytes',
					label: '单行缓冲上限',
					unit: 'bytes',
					min: RESOURCE_LIMIT_BOUNDS.fileLineBytes.min,
					max: RESOURCE_LIMIT_BOUNDS.fileLineBytes.max,
					danger: true,
					hint: '行读取、outline 和 summary 单行至少保留 1 字节，最多 128,000 字节。',
				},
				{
					key: 'file_summary_input_chars',
					label: '总结输入预算',
					unit: 'chars',
					max: RESOURCE_LIMIT_BOUNDS.fileSummaryInputChars.max,
					danger: true,
					hint: '发送给 small_model 的总结输入上限。调大 = 更多 token。',
				},
				{
					key: 'file_summary_timeout_secs',
					label: '总结超时',
					unit: 'secs',
					min: RESOURCE_LIMIT_BOUNDS.fileSummaryTimeoutSecs.min,
					max: RESOURCE_LIMIT_BOUNDS.fileSummaryTimeoutSecs.max,
					danger: false,
				},
				{
					key: 'file_max_list_entries',
					label: '目录列表条目上限',
					unit: 'count',
					min: RESOURCE_LIMIT_BOUNDS.fileListEntries.min,
					max: RESOURCE_LIMIT_BOUNDS.fileListEntries.max,
					danger: true,
					hint: '范围 0–10,000；设为 0 时目录列表返回空结果。',
				},
				{
					key: 'file_vision_max_bytes',
					label: '图片理解大小上限',
					unit: 'MiB',
					mb: true,
					max: RESOURCE_LIMIT_BOUNDS.fileOperationBytes.max / 1048576,
					danger: true,
					hint: '超过该大小的图片拒绝送视觉模型。',
				},
				{
					key: 'search_snippet_chars',
					label: '搜索片段长度',
					unit: 'chars',
					max: RESOURCE_LIMIT_BOUNDS.searchSnippetChars.max,
					danger: false,
				},
				{
					key: 'search_max_results',
					label: '搜索结果上限',
					unit: 'count',
					min: RESOURCE_LIMIT_BOUNDS.searchResults.min,
					max: RESOURCE_LIMIT_BOUNDS.searchResults.max,
					danger: true,
				},
				{
					key: 'search_max_file_size_bytes',
					label: '搜索跳过文件大小',
					unit: 'MiB',
					mb: true,
					max: RESOURCE_LIMIT_BOUNDS.searchFileSizeBytes.max / 1048576,
					hint: '最多跳过 1 GiB 以上的文件；设为 0 表示不限文件大小，单次请求不能绕过此配置上限。',
					danger: true,
				},
				{
					key: 'search_window_bytes',
					label: '行范围搜索窗口',
					unit: 'MiB',
					mb: true,
					min: RESOURCE_LIMIT_BOUNDS.searchWindowBytes.min / 1048576,
					max: RESOURCE_LIMIT_BOUNDS.searchWindowBytes.max / 1048576,
					danger: true,
					hint: '行范围窗口单次最多缓冲 16 MiB；设为 0 时回退到流式全文件扫描。',
				},
			],
		},
		{
			id: 'safety',
			title: '安全边界',
			hint: '外部输入与扩展（MCP、技能、脚本、网络）的防护上限。调大直接扩大攻击面，请谨慎。',
			fields: [
				{
					key: 'mcp_max_binary_base64_bytes',
					label: 'MCP Base64 内容上限',
					unit: 'MiB',
					mb: true,
					max: RESOURCE_LIMIT_BOUNDS.mcpBinaryBytes.max / 1048576,
					danger: true,
					hint: 'MCP image/audio/resource 的 Base64 表示按 UTF-8 字节数计限，超出替换为 oversized 标记。',
				},
				{
					key: 'mcp_max_sse_buffer_bytes',
					label: 'MCP SSE 缓冲上限',
					unit: 'MiB',
					mb: true,
					min: RESOURCE_LIMIT_BOUNDS.mcpSseBufferBytes.min / 1048576,
					max: RESOURCE_LIMIT_BOUNDS.mcpSseBufferBytes.max / 1048576,
					step: 1 / 16,
					displayDigits: 4,
					danger: true,
					hint: '单条未完成 SSE 事件缓冲上限，最多 4 MiB；设为 0 会丢弃所有 SSE 事件。',
				},
				{
					key: 'skills_max_md_bytes',
					label: 'SKILL.md 大小上限',
					unit: 'KiB',
					kb: true,
					min: RESOURCE_LIMIT_BOUNDS.skillMarkdownBytes.min / 1024,
					max: RESOURCE_LIMIT_BOUNDS.skillMarkdownBytes.max / 1024,
					step: 1,
					displayDigits: 0,
					danger: true,
					hint: '范围 0–1 MiB；设为 0 时所有 SKILL.md 都会被跳过。',
				},
				{
					key: 'skills_max_parse_lines',
					label: 'SKILL.md 解析行数',
					unit: 'lines',
					danger: true,
				},
				{
					key: 'skills_max_line_bytes',
					label: 'SKILL.md 单行长度',
					unit: 'bytes',
					danger: true,
				},
				{
					key: 'self_tool_max_instructions_bytes',
					label: '技能 instructions 上限',
					unit: 'KiB',
					kb: true,
					max: RESOURCE_LIMIT_BOUNDS.selfToolInstructionsBytes.max / 1024,
					danger: true,
				},
				{
					key: 'self_tool_max_script_bytes',
					label: '技能脚本大小上限',
					unit: 'KiB',
					kb: true,
					max: RESOURCE_LIMIT_BOUNDS.selfToolScriptBytes.max / 1024,
					danger: true,
				},
				{
					key: 'network_max_retries',
					label: '网络重试次数',
					unit: 'count',
					max: RESOURCE_LIMIT_BOUNDS.networkRetries.max,
					danger: true,
					hint: 'GET 请求的重试次数。调大 = 故障放大。',
				},
				{
					key: 'network_backoff_base_secs',
					label: '网络重试退避基数',
					unit: 'secs',
					max: RESOURCE_LIMIT_BOUNDS.networkBackoffSecs.max,
					danger: false,
				},
				{
					key: 'network_max_body_bytes',
					label: '网络响应体上限',
					unit: 'MiB',
					mb: true,
					min: RESOURCE_LIMIT_BOUNDS.networkBodyBytes.min / 1048576,
					max: RESOURCE_LIMIT_BOUNDS.networkBodyBytes.max / 1048576,
					step: 1 / 16,
					displayDigits: 4,
					danger: true,
					hint: 'HTTP 响应体缓冲范围 0–16 MiB；设为 0 时只保留状态和响应头信息。',
				},
			],
		},
		{
			id: 'agent',
			title: '代理循环行为',
			hint: 'ReAct 循环的结构化工具参数重试与流停滞反馈阈值。',
			fields: [
				{
					key: 'incomplete_tool_args_retries',
					label: '不完整工具参数重试次数',
					unit: 'count',
					danger: false,
					hint: '仅当工具调用参数不是完整 JSON 时，自动要求模型重新输出完整参数；其他响应错误显示“继续生成”。',
				},
				{
					key: 'stream_stall_warn_delay_ms',
					label: '流停滞提醒延迟',
					unit: 'ms',
					danger: false,
					hint: '流式输出停顿多久后向界面提示“仍在生成”。调大 = 更晚提示。',
				},
				{
					key: 'turn_deadline_secs',
					label: '单轮执行时限',
					unit: 'secs',
					danger: false,
					hint: '单次 Agent 运行允许持续的最长时间。',
				},
				{
					key: 'reasoning_echo_max_chars',
					label: '推理回显上限',
					unit: 'chars',
					danger: false,
					hint: '回传给服务商的每轮 reasoning 最大字符数（防止请求体过大导致流中断）。',
				},
			],
		},
		{
			id: 'resources',
			title: '资源上限',
			hint: '并发与内存资源保护。调大可能造成 CPU/内存/进程占用失控。',
			fields: [
				{
					key: 'max_running_background_tool_runs',
					label: '运行中后台任务数量上限',
					unit: 'count',
					min: CONCURRENCY_BOUNDS.backgroundToolRuns.min,
					max: CONCURRENCY_BOUNDS.backgroundToolRuns.max,
					danger: true,
						hint: '同时运行的后台工具调用数上限。调大可能增加资源占用。',
				},
				{
					key: 'foreground_tool_output_tail_max_chars',
					label: '前台工具输出尾部上限',
					unit: 'chars',
					danger: false,
					hint: '当前前台工具调用的实时输出预览保留字符数。',
				},
				{
					key: 'foreground_tool_output_emit_interval_ms',
					label: '前台输出事件间隔',
					unit: 'ms',
					danger: false,
				},
				{
					key: 'tool_run_terminal_ttl_secs',
					label: '后台任务保留时长',
					unit: 'secs',
					danger: false,
					hint: '已完成任务在面板保留多久后被回收（历史仍存数据库）。',
				},
				{
					key: 'clipboard_history_entries',
					label: '剪贴板历史默认条数',
					unit: 'count',
					max: RESOURCE_LIMIT_BOUNDS.clipboardEntries.max,
					danger: false,
					hint: '实际内存保留条数上限；设为 0 会清空历史并停止记录。',
				},
				{
					key: 'clipboard_history_max_entries',
					label: '剪贴板历史上限',
					unit: 'count',
					max: RESOURCE_LIMIT_BOUNDS.clipboardEntries.max,
					danger: true,
					hint: 'history 查询单次返回条数上限；设为 0 时查询返回空列表。',
				},
				{
					key: 'clipboard_entry_max_chars',
					label: '剪贴板条目截断',
					unit: 'chars',
					max: RESOURCE_LIMIT_BOUNDS.clipboardEntryChars.max,
					danger: false,
					hint: 'history 输出中每条剪贴板文本的截断长度；内存中每条记录另有固定安全上限。',
				},
				{
					key: 'event_chunk_batch_max_bytes',
					label: '事件分块批量上限',
					unit: 'KiB',
					kb: true,
					max: RESOURCE_LIMIT_BOUNDS.eventChunkBatchBytes.max / 1024,
					danger: false,
					hint: '最多 256 KiB；设为 0 时每个后续流分片立即单独发送。',
				},
				{
					key: 'input_ring_buffer_secs',
					label: '音频环形缓冲',
					unit: 'secs',
					min: RESOURCE_LIMIT_BOUNDS.inputRingBufferSecs.min,
					max: RESOURCE_LIMIT_BOUNDS.inputRingBufferSecs.max,
					danger: true,
					hint: '范围 0–120 秒。设为 0 使用采集默认容量；更改在下次创建录音引擎时生效，当前引擎不变。',
				},
				{
					key: 'embedding_chunk_size',
					label: '嵌入分块大小',
					unit: 'count',
					min: RESOURCE_LIMIT_BOUNDS.embeddingChunk.min,
					max: RESOURCE_LIMIT_BOUNDS.embeddingChunk.max,
					danger: false,
					hint: '范围 1–10，与单次 embedding provider 请求的硬上限一致。',
				},
			],
		},
	];

	const limitViews = LIMIT_GROUPS.map((g) => ({
		...g,
		normal: g.fields.filter((f) => !f.danger),
		danger: g.fields.filter((f) => f.danger),
	}));
	let limitDangerOpen = $state<Record<string, boolean>>(
		Object.fromEntries(limitViews.map((g) => [g.id, false])),
	);
	let allLimitDangerOpen = $derived(
		limitViews.every((g) => !g.danger.length || (limitDangerOpen[g.id] ?? true)),
	);

	function setAllLimitDanger(open: boolean) {
		for (const group of limitViews) limitDangerOpen[group.id] = open;
	}

	function limitDisplay(key: LimitKey, value: number | undefined) {
		const field = LIMIT_GROUPS.flatMap((g) => g.fields).find((x) => x.key === key);
		if (!field || value === undefined) return value ?? 0;
		const factor = 10 ** (field.displayDigits ?? 1);
		if (field.mb) return Math.round((value / 1048576) * factor) / factor;
		if (field.kb) return Math.round((value / 1024) * factor) / factor;
		if (field.kTokens) return Math.round(value / 1000);
		if (field.days) return Math.round((value / 86400) * 10) / 10;
		return value;
	}

	function limitCommit(key: LimitKey, value: number) {
		const field = LIMIT_GROUPS.flatMap((g) => g.fields).find((x) => x.key === key);
		if (!field) return value;
		if (field.mb) return Math.round(value * 1048576);
		if (field.kb) return Math.round(value * 1024);
		if (field.kTokens) return Math.round(value * 1000);
		if (field.days) return Math.round(value * 86400);
		return value;
	}

	function limitMaximum(field: LimitField): number {
		if (field.key !== 'compaction_reserve_tokens') return field.max ?? 100000000;
		const windows = [contextLimits.default_context_window, ...modelContextWindows].filter(
			(value): value is number =>
				typeof value === 'number' && Number.isSafeInteger(value) && value >= 1,
		);
		return Math.max(
			0,
			Math.min(field.max ?? 100000000, ...windows.map((window) => window - 1)),
		);
	}
</script>

<div class="limits-view">
	<div class="limits-toolbar md-toolbar">
		<p class="limits-legend">
			标记为<b>高影响</b>的项目会扩大内存、成本或安全边界，默认收起；需要时可按分类展开。
		</p>
		<MaterialButton
			variant="text"
			className="limit-toggle-all"
			ariaLabel={allLimitDangerOpen ? '折叠全部高影响项目' : '展开全部高影响项目'}
			ariaExpanded={allLimitDangerOpen}
			onclick={() => setAllLimitDanger(!allLimitDangerOpen)}
		>
			<span class="limit-danger-caret" aria-hidden="true">
				<Icon name="chevronDown" size={12} strokeWidth={2.5} />
			</span>
			<span>{allLimitDangerOpen ? '折叠全部高影响项目' : '展开全部高影响项目'}</span>
		</MaterialButton>
	</div>
	<div class="limits-grid">
		{#each limitViews as group}
			<MaterialCard variant="outlined" className="format-card limit-card">
				<h3>{group.title}</h3>
				<p class="model-hint">{group.hint}</p>
				{#each group.normal as f}
					<div
						class="form-row limit-row settings-field-layout"
						class:danger-row={f.danger}
						data-setting-searchable
					>
						<div class="limit-label">
							<label for="limit-{f.key}">{f.label}</label>{#if f.danger}<MaterialBadge
									text="⚠ 危险"
									variant="error"
									className="danger-badge"
									title={f.hint || '调整此值存在内存 / 成本 / 安全风险'}
								/>{/if}{#if f.hint}<p class="limit-hint">{f.hint}</p>{/if}
						</div>
						<MaterialNumberFieldWithUnit
							width="standard"
							id="limit-{f.key}"
							value={limitDisplay(f.key, contextLimits[f.key])}
							step={f.step ?? 1}
							min={f.min ?? 0}
							max={limitMaximum(f)}
							unit={f.unit}
							onChange={withNumberValue((v) => {
								contextLimits[f.key] = limitCommit(f.key, v);
							})}
						/>
					</div>
				{/each}
				{#if group.danger.length}
					<div class="limit-danger-box">
						<MaterialCollapsible variant="error" bind:open={limitDangerOpen[group.id]}>
							{#snippet header()}<MaterialBadge
									text="高影响项目"
									variant="error"
									className="danger-badge"
								/><span
									class="limit-danger-count">{group.danger.length} 项</span
								>{/snippet}
							<div class="limit-danger-items">
								{#each group.danger as f}
									<div
									class="form-row limit-row settings-field-layout"
									data-setting-searchable
								>
										<div class="limit-label">
											<label for="limit-{f.key}">{f.label}</label
											>{#if f.hint}<p class="limit-hint">{f.hint}</p>{/if}
										</div>
										<MaterialNumberFieldWithUnit
											width="standard"
											id="limit-{f.key}"
											value={limitDisplay(f.key, contextLimits[f.key])}
											step={f.step ?? 1}
											min={f.min ?? 0}
											max={limitMaximum(f)}
											unit={f.unit}
											onChange={withNumberValue((v) => {
												contextLimits[f.key] = limitCommit(f.key, v);
											})}
										/>
									</div>
								{/each}
							</div>
						</MaterialCollapsible>
					</div>
				{/if}
			</MaterialCard>
		{/each}
	</div>
</div>

<style>
	:global(.format-card) {
		background: var(--md-sys-color-surface-container-low);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		padding: var(--md-sys-space-md);
	}
	:global(.format-card h3) {
		font-size: var(--md-sys-typescale-body-large-size);
		font-weight: 600;
		line-height: var(--md-sys-typescale-body-large-line-height);
		color: var(--md-sys-color-primary);
		margin-bottom: var(--md-sys-space-sm);
	}
	:global(.format-card .model-hint) {
		margin-top: 0;
		margin-bottom: var(--md-sys-space-md);
	}
	.limits-grid {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: var(--md-sys-space-md);
	}
	:global(.limit-card) {
		min-width: 0;
	}
	.limits-toolbar {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-comp-toolbar-gap);
		flex-wrap: wrap;
		margin-bottom: var(--md-sys-space-md);
	}
	.limits-legend {
		font-size: var(--md-sys-typescale-label-medium-size);
		line-height: var(--md-sys-typescale-label-medium-line-height);
		color: var(--md-sys-color-on-surface-variant);
		margin: 0;
	}
	.limits-legend b {
		color: var(--md-sys-color-error);
		font-weight: 600;
	}
	.limit-row {
		min-width: 0;
	}
	.limit-danger-box {
		border: 1px solid var(--md-sys-color-error);
		border-radius: var(--md-sys-shape-small);
		background: color-mix(in srgb, var(--md-sys-color-error) 6%, transparent);
		margin-top: var(--md-sys-space-sm);
		padding: var(--md-sys-space-sm);
	}
	.limit-danger-items {
		display: grid;
		gap: var(--md-sys-space-md);
	}
	.limit-danger-caret {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		flex-shrink: 0;
		color: var(--md-sys-color-error);
		transition: transform 0.15s ease;
	}
	:global(.limit-toggle-all) {
		display: inline-flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
	}
	:global(.limit-toggle-all[aria-expanded='false'] .limit-danger-caret) {
		transform: rotate(-90deg);
	}
	.limit-danger-count {
		font-size: var(--md-sys-typescale-label-small-size);
		line-height: var(--md-sys-typescale-label-small-line-height);
		color: var(--md-sys-color-on-surface-variant);
		margin-left: auto;
	}
	.limit-label {
		min-width: 0;
	}
	.limit-label label {
		font-size: var(--md-sys-typescale-body-small-size);
		font-weight: 500;
		line-height: var(--md-sys-typescale-body-small-line-height);
	}
	.limit-hint {
		font-size: var(--md-sys-typescale-label-small-size);
		color: var(--md-sys-color-on-surface-variant);
		margin: var(--md-sys-space-2xs) 0 0;
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	:global(.danger-badge) {
		margin-left: var(--md-sys-space-xs);
		vertical-align: 1px;
	}
	.model-hint {
		font-size: var(--md-sys-typescale-label-small-size);
		color: var(--md-sys-color-on-surface-variant);
		margin-top: 0;
		margin-bottom: var(--md-sys-space-md);
		line-height: var(--md-sys-typescale-label-small-line-height);
	}
	.form-row {
		margin-bottom: var(--md-sys-space-sm);
	}
</style>
