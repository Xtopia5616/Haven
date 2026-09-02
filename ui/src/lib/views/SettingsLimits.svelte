<script>
	import MaterialCollapsible from '$lib/MaterialCollapsible.svelte';
	import MaterialNumberField from '$lib/MaterialNumberField.svelte';
	import { withNumberValue } from '$lib/typedCallbacks.js';

	let { contextLimits } = $props();

	/** @type {any[]} */
	const LIMIT_GROUPS = [
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
					danger: true,
					hint: '角色未填写 Context 且 Provider /models 未返回上下文长度时的回退窗口（单位 K = 1000 tokens，如 128 = 128K）。调高会增大每次请求的成本与溢出风险。',
				},
				{
					key: 'max_response_tokens',
					label: '回复输出 token 下限',
					unit: 'tokens',
					danger: false,
					hint: '每个模型端点的 max_tokens 会被抬到不低于此值（取两者较大）。默认极大，长回复不会被截断；需要限制输出长度时调低此项。',
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
					danger: false,
					hint: '计算压缩阈值时为模型回复预留的 token 数。',
				},
				{
					key: 'max_observation_chars',
					label: '工具观察字符上限',
					unit: 'chars',
					danger: true,
					hint: '工具结果进入对话的最大字符数，也是 shell/file/process 等工具的默认输出截断上限（per-tool 可覆盖）。调大直接推高 token 成本。',
				},
				{
					key: 'max_tools_per_request',
					label: '单次请求工具数上限',
					unit: 'count',
					danger: true,
					hint: '发给模型的 tools 数组最大长度。默认 128（提供方硬顶约 350）。内置工具优先保留；load_mcp 可按 tool_names 只加载子集，整服超限时返回工具目录供再选。',
				},
				{
					key: 'max_transcript_chars',
					label: '记忆提取转录上限',
					unit: 'chars',
					danger: true,
					hint: '事实提取时发送给模型的转录长度。',
				},
				{
					key: 'notification_summary_chars',
					label: '通知摘要字符上限',
					unit: 'chars',
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
					hint: '长会话每多少步重新做一次事实提取。调小增加调用成本。',
				},
				{ key: 'max_known_facts', label: '提示中已知事实数', unit: 'count', danger: false },
				{
					key: 'sanitize_field_max_chars',
					label: '事实字段消毒长度',
					unit: 'chars',
					danger: true,
					hint: '事实字段注入到系统提示前的截断长度。调大 = 更大提示注入面。',
				},
			],
		},
		{
			id: 'files',
			title: '文件与搜索工具',
			hint: 'files 工具读取、总结与搜索的资源上限。',
			fields: [
				{
					key: 'file_read_max_chars',
					label: '文件全读上限',
					unit: 'chars',
					danger: true,
					hint: '超过该大小的文件不整体读取，改用 offset/limit 分段。调大 = 大文件整读内存风险。',
				},
				{
					key: 'file_max_byte_read',
					label: '字节读取绝对上限',
					unit: 'bytes',
					mb: true,
					danger: true,
					hint: 'byte 模式单次读取的安全上限（不受调用方 limit 影响）。',
				},
				{ key: 'file_line_span', label: '行模式默认跨度', unit: 'lines', danger: false },
				{
					key: 'file_max_line_chars',
					label: '单行缓冲上限',
					unit: 'chars',
					danger: true,
					hint: '病态单行文件（压缩包/超长行）的缓冲上限。',
				},
				{
					key: 'file_summary_input_chars',
					label: '总结输入预算',
					unit: 'chars',
					danger: true,
					hint: '发送给 small_model 的总结输入上限。调大 = 更多 token。',
				},
				{
					key: 'file_summary_timeout_secs',
					label: '总结超时',
					unit: 'secs',
					danger: false,
				},
				{
					key: 'file_max_list_entries',
					label: '目录列表条目上限',
					unit: 'count',
					danger: true,
				},
				{
					key: 'file_vision_max_bytes',
					label: '图片理解大小上限',
					unit: 'MB',
					mb: true,
					danger: true,
					hint: '超过该大小的图片拒绝送视觉模型。',
				},
				{
					key: 'search_snippet_chars',
					label: '搜索片段长度',
					unit: 'chars',
					danger: false,
				},
				{ key: 'search_max_results', label: '搜索结果上限', unit: 'count', danger: true },
				{
					key: 'search_max_file_size_bytes',
					label: '搜索跳过文件大小',
					unit: 'MB',
					mb: true,
					danger: true,
				},
				{
					key: 'search_window_bytes',
					label: '行范围搜索窗口',
					unit: 'MB',
					mb: true,
					danger: true,
				},
			],
		},
		{
			id: 'safety',
			title: '安全边界',
			hint: '外部输入与扩展（MCP、技能、脚本、网络）的防护上限。调大直接扩大攻击面，请谨慎。',
			fields: [
				{
					key: 'mcp_max_binary_payload_bytes',
					label: 'MCP 二进制内容上限',
					unit: 'MB',
					mb: true,
					danger: true,
					hint: 'MCP image/audio/resource 内容保留在观察中的 base64 上限，超出替换为 oversized 标记。',
				},
				{
					key: 'mcp_max_sse_buffer_bytes',
					label: 'MCP SSE 缓冲上限',
					unit: 'MB',
					mb: true,
					danger: true,
					hint: '单条未完成 SSE 事件缓冲上限，防恶意服务器无限增长。',
				},
				{
					key: 'skills_max_md_bytes',
					label: 'SKILL.md 大小上限',
					unit: 'KB',
					kb: true,
					danger: true,
					hint: '超过该大小的技能描述文件被跳过（防 OOM）。',
				},
				{
					key: 'skills_max_parse_lines',
					label: 'SKILL.md 解析行数',
					unit: 'lines',
					danger: true,
				},
				{
					key: 'skills_max_line_len',
					label: 'SKILL.md 单行长度',
					unit: 'chars',
					danger: true,
				},
				{
					key: 'self_tool_max_instructions_bytes',
					label: '技能 instructions 上限',
					unit: 'KB',
					kb: true,
					danger: true,
				},
				{
					key: 'self_tool_max_script_bytes',
					label: '技能脚本大小上限',
					unit: 'KB',
					kb: true,
					danger: true,
				},
				{
					key: 'network_max_retries',
					label: '网络重试次数',
					unit: 'count',
					danger: true,
					hint: 'GET 请求的重试次数。调大 = 故障放大。',
				},
				{
					key: 'network_backoff_base_secs',
					label: '网络重试退避基数',
					unit: 'secs',
					danger: false,
				},
				{
					key: 'network_max_body_bytes',
					label: '网络响应体上限',
					unit: 'MB',
					mb: true,
					danger: true,
				},
			],
		},
		{
			id: 'agent',
			title: '代理循环行为',
			hint: 'ReAct 循环的重试/空响应/停滞反馈阈值。这些原本是硬编码常量，现可在设置中调整。',
			fields: [
				{
					key: 'cut_off_retries',
					label: '截断回复重试次数',
					unit: 'count',
					danger: false,
					hint: '看起来被截断/中途停止的文字回复会带提示重试几次再作为最终答案。调大 = 更努力地补全长回复。',
				},
				{
					key: 'empty_response_max_retries',
					label: '空响应重试次数',
					unit: 'count',
					danger: false,
					hint: '完全空的模型响应重试几次再报错（服务端静默失败的兜底）。',
				},
				{
					key: 'empty_response_retry_delay_ms',
					label: '空响应重试间隔',
					unit: 'ms',
					danger: false,
				},
				{
					key: 'stream_stall_warn_delay_ms',
					label: '流停滞提醒延迟',
					unit: 'ms',
					danger: false,
					hint: '流式输出停顿多久后向界面提示“仍在生成”。调大 = 更晚提示。',
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
					key: 'background_max_actions',
					label: '后台任务并发上限',
					unit: 'count',
					danger: true,
					hint: '同时运行的 background shell 任务数。调大 = 子进程失控风险。',
				},
				{
					key: 'background_job_tail_max_chars',
					label: '后台任务输出尾部上限',
					unit: 'chars',
					danger: false,
					hint: '运行中任务实时输出预览保留的尾部字符数。',
				},
				{
					key: 'background_job_output_emit_interval_ms',
					label: '后台输出事件间隔',
					unit: 'ms',
					danger: false,
				},
				{
					key: 'terminal_job_ttl_secs',
					label: '后台任务保留时长',
					unit: 'secs',
					danger: false,
					hint: '已完成任务在面板保留多久后被回收（历史仍存数据库）。',
				},
				{
					key: 'scheduled_actions_max',
					label: '定时任务数量上限',
					unit: 'count',
					danger: true,
				},
				{
					key: 'scheduled_actions_due_horizon_secs',
					label: '定时任务最远排期',
					unit: 'days',
					days: true,
					danger: false,
				},
				{
					key: 'clipboard_history_entries',
					label: '剪贴板历史默认条数',
					unit: 'count',
					danger: false,
				},
				{
					key: 'clipboard_history_max_entries',
					label: '剪贴板历史上限',
					unit: 'count',
					danger: true,
				},
				{
					key: 'clipboard_entry_max_chars',
					label: '剪贴板条目截断',
					unit: 'chars',
					danger: false,
				},
				{
					key: 'event_chunk_batch_max_bytes',
					label: '事件分块批量上限',
					unit: 'KB',
					kb: true,
					danger: false,
					hint: 'agent 流式事件聚合分块的大小（IPC 频率与延迟权衡）。',
				},
				{
					key: 'input_ring_buffer_secs',
					label: '音频环形缓冲',
					unit: 'secs',
					danger: true,
					hint: '录音缓冲时长。调大 = 内存增加 + 停止录音后仍会处理更长音频。',
				},
				{
					key: 'embedding_chunk_size',
					label: '嵌入分块大小',
					unit: 'count',
					danger: false,
					hint: 'embedding 请求分块（提供方限制）。',
				},
			],
		},
	];

	const limitViews = LIMIT_GROUPS.map((/** @type {any} */ g) => ({
		...g,
		normal: g.fields.filter((/** @type {any} */ f) => !f.danger),
		danger: g.fields.filter((/** @type {any} */ f) => f.danger),
	}));
	let limitDangerOpen = $state(Object.fromEntries(limitViews.map((g) => [g.id, true])));
	let allLimitDangerOpen = $derived(
		limitViews.every((g) => !g.danger.length || (limitDangerOpen[g.id] ?? true)),
	);

	/** @param {boolean} open */
	function setAllLimitDanger(open) {
		for (const group of limitViews) limitDangerOpen[group.id] = open;
	}

	/** @param {string} key @param {number} value */
	function limitDisplay(key, value) {
		const field = LIMIT_GROUPS.flatMap((g) => g.fields).find((x) => x.key === key);
		if (!field) return value;
		if (field.mb) return Math.round((value / 1048576) * 10) / 10;
		if (field.kb) return Math.round((value / 1024) * 10) / 10;
		if (field.kTokens) return Math.round(value / 1000);
		if (field.days) return Math.round((value / 86400) * 10) / 10;
		return value;
	}

	/** @param {string} key @param {number} value */
	function limitCommit(key, value) {
		const field = LIMIT_GROUPS.flatMap((g) => g.fields).find((x) => x.key === key);
		if (!field) return value;
		if (field.mb) return Math.round(value * 1048576);
		if (field.kb) return Math.round(value * 1024);
		if (field.kTokens) return Math.round(value * 1000);
		if (field.days) return Math.round(value * 86400);
		return value;
	}
</script>

<div class="limits-view">
	<div class="limits-toolbar">
		<p class="limits-legend">
			红色边框为<b>危险项</b>：调大会扩大内存 / 成本 / 攻击面，默认排在每组底部，可折叠。
		</p>
		<button
			class="md-btn md-btn--text limit-toggle-all"
			onclick={() => setAllLimitDanger(!allLimitDangerOpen)}
			aria-expanded={allLimitDangerOpen}
		>
			<span class="limit-danger-caret" aria-hidden="true"
				><svg
					width="12"
					height="12"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2.5"
					stroke-linecap="round"
					stroke-linejoin="round"><polyline points="6 9 12 15 18 9" /></svg
				></span
			>
			{allLimitDangerOpen ? '折叠全部危险项' : '展开全部危险项'}
		</button>
	</div>
	<div class="limits-grid">
		{#each limitViews as group}
			<div class="format-card limit-card">
				<h3>{group.title}</h3>
				<p class="model-hint">{group.hint}</p>
				{#each group.normal as f}
					<div class="form-row limit-row" class:danger-row={f.danger}>
						<div class="limit-label">
							<label for="limit-{f.key}">{f.label}</label>{#if f.danger}<span
									class="danger-badge"
									title={f.hint || '调整此值存在内存 / 成本 / 安全风险'}
									>⚠ 危险</span
								>{/if}{#if f.hint}<p class="limit-hint">{f.hint}</p>{/if}
						</div>
						<div class="limit-input">
							<MaterialNumberField
								id="limit-{f.key}"
								value={limitDisplay(f.key, contextLimits[f.key])}
								step={f.step ?? 1}
								min={f.min ?? 0}
								max={f.max ?? 100000000}
								onChange={withNumberValue((v) => {
									contextLimits[f.key] = limitCommit(f.key, v);
								})}
							/><span class="limit-unit">{f.unit}</span>
						</div>
					</div>
				{/each}
				{#if group.danger.length}
					<div class="limit-danger-box">
						<MaterialCollapsible variant="error" bind:open={limitDangerOpen[group.id]}>
							{#snippet header()}<span class="danger-badge">⚠ 危险项</span><span
									class="limit-danger-count">{group.danger.length} 项</span
								>{/snippet}
							<div class="limit-danger-items">
								{#each group.danger as f}
									<div class="form-row limit-row">
										<div class="limit-label">
											<label for="limit-{f.key}">{f.label}</label
											>{#if f.hint}<p class="limit-hint">{f.hint}</p>{/if}
										</div>
										<div class="limit-input">
											<MaterialNumberField
												id="limit-{f.key}"
												value={limitDisplay(f.key, contextLimits[f.key])}
												step={f.step ?? 1}
												min={f.min ?? 0}
												max={f.max ?? 100000000}
												onChange={withNumberValue((v) => {
													contextLimits[f.key] = limitCommit(f.key, v);
												})}
											/><span class="limit-unit">{f.unit}</span>
										</div>
									</div>
								{/each}
							</div>
						</MaterialCollapsible>
					</div>
				{/if}
			</div>
		{/each}
	</div>
</div>

<style>
	.format-card {
		background: var(--md-sys-color-surface-container-lowest);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		padding: var(--md-sys-space-md);
		margin-bottom: var(--md-sys-space-md);
	}
	.format-card h3 {
		font-size: 14px;
		font-weight: 600;
		color: var(--md-sys-color-primary);
		margin-bottom: var(--md-sys-space-sm);
	}
	.format-card .model-hint {
		margin-top: 0;
		margin-bottom: var(--md-sys-space-md);
	}
	.limits-grid {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: var(--md-sys-space-md);
	}
	.limit-card {
		min-width: 0;
	}
	.limits-toolbar {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--md-sys-space-md);
		flex-wrap: wrap;
		margin-bottom: var(--md-sys-space-md);
	}
	.limits-legend {
		font-size: 12px;
		color: var(--md-sys-color-on-surface-variant);
		margin: 0;
	}
	.limits-legend b {
		color: var(--md-sys-color-error, #ba1a1a);
		font-weight: 600;
	}
	.limit-row {
		display: flex;
		justify-content: space-between;
		align-items: flex-start;
		gap: var(--md-sys-space-md);
	}
	.limit-danger-box {
		border: 1px solid var(--md-sys-color-error, #ba1a1a);
		border-radius: var(--md-sys-shape-small);
		background: color-mix(in srgb, var(--md-sys-color-error, #ba1a1a) 6%, transparent);
		margin-top: var(--md-sys-space-sm);
		padding: 8px;
	}
	.limit-danger-items {
		display: grid;
		gap: 10px;
	}
	.limit-danger-caret {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		flex-shrink: 0;
		color: var(--md-sys-color-error, #ba1a1a);
		transition: transform 0.15s ease;
	}
	.limit-toggle-all {
		display: inline-flex;
		align-items: center;
		gap: 4px;
	}
	.limit-toggle-all[aria-expanded='false'] .limit-danger-caret {
		transform: rotate(-90deg);
	}
	.limit-danger-count {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		margin-left: auto;
	}
	.limit-label {
		flex: 1;
		min-width: 0;
	}
	.limit-label label {
		font-size: 13px;
		font-weight: 500;
	}
	.limit-hint {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		margin: 2px 0 0;
	}
	.danger-badge {
		display: inline-block;
		margin-left: 6px;
		padding: 1px 6px;
		border-radius: 999px;
		font-size: 10px;
		font-weight: 600;
		color: #fff;
		background: var(--md-sys-color-error, #ba1a1a);
		vertical-align: 1px;
	}
	.limit-input {
		display: flex;
		align-items: center;
		gap: 6px;
	}
	.limit-unit {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		min-width: 42px;
	}
	.model-hint {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		margin-top: calc(-1 * var(--md-sys-space-sm));
		margin-bottom: var(--md-sys-space-md);
	}
	.form-row {
		display: flex;
		align-items: center;
		margin-bottom: var(--md-sys-space-sm);
		gap: var(--md-sys-space-md);
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
		.limit-input {
			justify-content: space-between;
		}
		.format-card {
			padding: var(--md-sys-space-sm);
		}
	}
</style>
