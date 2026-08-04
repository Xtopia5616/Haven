<script module>
	// Structured renderers for tool observations whose output is JSON, plus the
	// `ask` question card. Kept in one place so live chat and history review
	// share the same cards with a unified look.

	/** @type {Record<string, string>} */
	const LABELS = {
		search: '文件搜索',
		process: '进程列表',
		window: '窗口列表',
		status: '后台任务',
		reminder: '提醒',
		env: '环境变量',
		file: '文件操作',
		network: '网络请求',
		clipboard: '剪贴板',
		power: '电源状态',
		system: '系统信息',
	};

	/** @param {unknown} v */
	function isObj(v) {
		return typeof v === 'object' && v !== null && !Array.isArray(v);
	}

	/** @param {string | number} v */
	function clampPct(v) {
		const n = Number(v);
		if (!Number.isFinite(n)) return 0;
		return Math.max(0, Math.min(100, n));
	}

	/** @param {unknown} v */
	function fmtBytes(v) {
		const n = Number(v);
		if (!Number.isFinite(n) || n < 0) return '—';
		if (n < 1024) return `${n} B`;
		const units = ['KB', 'MB', 'GB', 'TB'];
		let u = n;
		let i = -1;
		while (u >= 1024 && i < units.length - 1) {
			u /= 1024;
			i++;
		}
		return `${u >= 100 ? u.toFixed(0) : u.toFixed(1)} ${units[i]}`;
	}

	/** @param {unknown} v */
	function fmtUptime(v) {
		const secs = Number(v);
		if (!Number.isFinite(secs) || secs < 0) return null;
		const d = Math.floor(secs / 86400);
		const h = Math.floor((secs % 86400) / 3600);
		const m = Math.floor((secs % 3600) / 60);
		if (d > 0) return `${d} 天 ${h} 小时`;
		if (h > 0) return `${h} 小时 ${m} 分`;
		return `${m} 分钟`;
	}

	/**
	 * Whether this tool observation can be rendered as a structured card.
	 * @param {string} toolName
	 * @param {string} content
	 * @returns {boolean}
	 */
	export function canRenderToolResult(toolName, content) {
		return parseToolResult(toolName, content) !== null;
	}

	/**
	 * Parse + validate a tool observation into a renderable payload.
	 * @param {string} toolName
	 * @param {string} content
	 * @returns {object | null}
	 */
	export function parseToolResult(toolName, content) {
		if (!content) return null;
		let data;
		try {
			data = JSON.parse(content);
		} catch {
			return null;
		}
		if (!isObj(data)) return null;
		switch (toolName) {
			case 'search':
				return Array.isArray(data.results) ? { data } : null;
			case 'system':
				return data.cpu || data.memory || data.os || data.disks ? { data } : null;
			case 'process':
				return Array.isArray(data.processes) ? { data } : null;
			case 'window':
				return Array.isArray(data.windows) ? { data } : null;
			case 'status':
				return typeof data.status === 'string' ? { data } : null;
			case 'reminder':
				return Array.isArray(data.reminders) || (data.id && data.mode) ? { data } : null;
			case 'env':
				return Array.isArray(data.variables) || data.name ? { data } : null;
			case 'file':
				return data.written || data.edited || data.copied || data.moved || data.deleted ||
					Array.isArray(data.entries) || 'content' in data || 'size' in data
					? { data }
					: null;
			case 'network':
				return typeof data.status === 'number' ? { data } : null;
			case 'clipboard':
				return 'content' in data || data.written === true ? { data } : null;
			case 'power':
				return 'battery_percent' in data || data.locked || data.sleep || data.hibernate
					? { data }
					: null;
			default:
				return null;
		}
	}
</script>

<script>
	let {
		type = 'tool',
		toolName = '',
		content = '',
		options = [],
		awaiting = false,
		messageId = '',
		onQuickReply = null,
	} = $props();
	let parsed = $derived(type === 'tool' ? parseToolResult(toolName, content) : null);
	let data = $derived(parsed?.data ?? {});
</script>

{#if type === 'ask'}
	<div class="tool-card" role="status">
		<div class="tool-card-header">
			<span class="tool-card-icon" aria-hidden="true">&#63;</span>
			<span class="tool-card-label">Haven 需要你确认</span>
		</div>
		{#if content}
			<p class="ask-question">{content}</p>
		{/if}
		{#if options && options.length > 0 && awaiting}
			<div class="ask-options">
				{#each options as opt (opt)}
					<button
						class="ask-option"
						onclick={() => onQuickReply?.(messageId, opt)}
						type="button"
					>{opt}</button>
				{/each}
			</div>
		{/if}
		{#if awaiting}
			<div class="ask-waiting">
				<span class="ask-waiting-dot"></span>
				等待你的回答...
			</div>
		{/if}
	</div>
{:else if parsed}
	<div class="tool-card" role="status">
		<div class="tool-card-header">
			<span class="tool-card-icon" aria-hidden="true">
				{#if toolName === 'search'}
					<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round"><circle cx="11" cy="11" r="7" /><line x1="21" y1="21" x2="16.65" y2="16.65" /></svg>
				{:else if toolName === 'system'}
					<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><rect x="4" y="4" width="16" height="16" rx="2" /><rect x="9" y="9" width="6" height="6" /><path d="M9 1v3M15 1v3M9 20v3M15 20v3M20 9h3M20 15h3M1 9h3M1 15h3" /></svg>
				{:else if toolName === 'process'}
					<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="22 12 18 12 15 21 9 3 6 12 2 12" /></svg>
				{:else if toolName === 'window'}
					<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="3" width="20" height="14" rx="2" /><path d="M8 21h8M12 17v4" /></svg>
				{:else if toolName === 'status'}
					<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="9" /><polyline points="12 7 12 12 15.5 13.5" /></svg>
				{:else if toolName === 'reminder'}
					<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><path d="M18 8a6 6 0 0 0-12 0c0 7-3 9-3 9h18s-3-2-3-9" /><path d="M13.73 21a2 2 0 0 1-3.46 0" /></svg>
				{:else if toolName === 'env'}
					<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><path d="M8 3H7a2 2 0 0 0-2 2v5a2 2 0 0 1-2 2 2 2 0 0 1 2 2v5c0 1.1.9 2 2 2h1" /><path d="M16 21h1a2 2 0 0 0 2-2v-5c0-1.1.9-2 2-2a2 2 0 0 1-2-2V5a2 2 0 0 0-2-2h-1" /></svg>
				{:else if toolName === 'file'}
					<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" /><polyline points="14 2 14 8 20 8" /></svg>
				{:else if toolName === 'network'}
					<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10" /><path d="M2 12h20M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z" /></svg>
				{:else if toolName === 'clipboard'}
					<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><path d="M16 4h2a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2" /><rect x="8" y="2" width="8" height="4" rx="1" /></svg>
				{:else if toolName === 'power'}
					<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><rect x="1" y="6" width="18" height="12" rx="2" /><line x1="23" y1="10" x2="23" y2="14" /><line x1="5" y1="10" x2="5" y2="14" /></svg>
				{/if}
			</span>
			<span class="tool-card-label">{LABELS[toolName] ?? toolName}</span>
		</div>

		{#if toolName === 'search'}
			<div class="tool-card-count">
				{data.count ?? data.results.length} 个结果 · {data.mode === 'content' ? '全文' : '文件名'}
			</div>
			{#if data.results.length > 0}
				<div class="tool-card-list">
					{#each data.results as r (r.path + (r.line ?? ''))}
						<div class="search-row">
							<span class="search-path">{r.path}</span>
							{#if r.line != null}
								<span class="search-line">L{r.line}</span>
								<span class="search-snippet">{r.snippet ?? ''}</span>
							{/if}
						</div>
					{/each}
				</div>
			{:else}
				<p class="tool-card-empty">没有匹配的结果</p>
			{/if}
		{:else if toolName === 'system'}
			{#if data.os}
				<div class="sys-os">
					<span class="sys-os-name">{data.os.name || '系统'}</span>
					{#if data.os.hostname}<span class="sys-os-host">{data.os.hostname}</span>{/if}
				</div>
			{/if}
			{#if data.cpu}
				<div class="meter-row">
					<span class="meter-label">CPU</span>
					<span class="meter-value">{Number(data.cpu.usage_pct ?? 0).toFixed(1)}%</span>
					<span class="meter-track"><span class="meter-fill" style="width: {clampPct(data.cpu.usage_pct)}%"></span></span>
					<span class="meter-sub">{data.cpu.cores ?? 0} 核 / {data.cpu.logical_cpus ?? 0} 线程</span>
				</div>
			{/if}
			{#if data.memory}
				<div class="meter-row">
					<span class="meter-label">内存</span>
					<span class="meter-value">{fmtBytes(data.memory.used_bytes)} / {fmtBytes(data.memory.total_bytes)}</span>
					<span class="meter-track"><span class="meter-fill" style="width: {clampPct((Number(data.memory.used_bytes) / Math.max(Number(data.memory.total_bytes), 1)) * 100)}%"></span></span>
				</div>
			{/if}
			{#if Array.isArray(data.disks)}
				{#each data.disks as d (d.mount)}
					<div class="meter-row">
						<span class="meter-label">{d.mount}</span>
						<span class="meter-value">{fmtBytes(Number(d.total_bytes) - Number(d.available_bytes))} / {fmtBytes(d.total_bytes)}</span>
						<span class="meter-track"><span class="meter-fill" style="width: {clampPct((1 - Number(d.available_bytes) / Math.max(Number(d.total_bytes), 1)) * 100)}%"></span></span>
					</div>
				{/each}
			{/if}
			{#if data.os?.uptime_secs != null}
				<div class="tool-card-meta">运行时长 {fmtUptime(data.os.uptime_secs)}</div>
			{/if}
		{:else if toolName === 'process'}
			<div class="tool-card-count">{data.processes.length} 个进程</div>
			<div class="tool-card-list">
				<table class="proc-table">
					<thead>
						<tr><th>进程</th><th>PID</th><th>CPU</th><th>内存</th></tr>
					</thead>
					<tbody>
						{#each data.processes as p (p.pid)}
							<tr>
								<td class="proc-name" title={p.name}>{p.name}</td>
								<td class="proc-num">{p.pid}</td>
								<td class="proc-num">{Number(p.cpu ?? 0).toFixed(1)}%</td>
								<td class="proc-num">{fmtBytes(p.memory)}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{:else if toolName === 'window'}
			<div class="tool-card-count">{data.count ?? data.windows.length} 个窗口</div>
			{#if data.windows.length > 0}
				<div class="tool-card-list">
					{#each data.windows as w (w.hwnd ?? w.title)}
						<div class="window-row">
							<span class="window-title" title={w.title}>{w.title || '(无标题)'}</span>
							{#if w.pid}<span class="window-pid">PID {w.pid}</span>{/if}
						</div>
					{/each}
				</div>
			{:else}
				<p class="tool-card-empty">没有可见窗口</p>
			{/if}
		{:else if toolName === 'status'}
			<div class="job-row">
				<span class="job-id">{data.job_id}</span>
				<span class="status-badge status-{data.status}">{data.status}</span>
			</div>
			{#if data.exit_code != null}
				<div class="tool-card-meta">退出码 {data.exit_code}</div>
			{/if}
		{:else if toolName === 'reminder'}
			{#if Array.isArray(data.reminders)}
				<div class="tool-card-count">{data.reminders.length} 条提醒</div>
				{#if data.reminders.length > 0}
					<div class="tool-card-list">
						{#each data.reminders as r (r.id)}
							<div class="reminder-row">
								<span class="reminder-title">{r.title || r.body}</span>
								{#if r.mode}
									<span class="reminder-mode">{r.mode}</span>
								{/if}
								{#if r.fires_at}
									<span class="reminder-time">{r.fires_at}</span>
								{/if}
							</div>
						{/each}
					</div>
				{:else}
					<p class="tool-card-empty">没有待触发的提醒</p>
				{/if}
			{:else}
				<div class="job-row">
					<span class="job-id">#{data.id}</span>
					<span class="reminder-mode">{data.mode}</span>
				</div>
				{#if data.fires_at}
					<div class="tool-card-meta">触发时间 {data.fires_at}</div>
				{/if}
			{/if}
		{:else if toolName === 'env'}
			{#if Array.isArray(data.variables)}
				<div class="tool-card-count">{data.variables.length} 个变量</div>
				{#if data.variables.length > 0}
					<div class="tool-card-list">
						{#each data.variables as v (v.name)}
							<div class="env-row">
								<span class="env-name">{v.name}</span>
								<span class="env-value" title={v.value ?? ''}>{v.value ?? '(未设置)'}</span>
							</div>
						{/each}
					</div>
				{:else}
					<p class="tool-card-empty">（空）</p>
				{/if}
			{:else}
				<div class="env-row">
					<span class="env-name">{data.name}</span>
					<span class="env-value" title={data.value ?? ''}>{data.value ?? '(未设置)'}</span>
				</div>
			{/if}
		{:else if toolName === 'file'}
			{#if data.written}
				<div class="file-row"><span class="file-op">已写入</span><span class="file-path">{data.path}</span></div>
			{:else if data.edited}
				<div class="file-row"><span class="file-op">已编辑</span><span class="file-path">{data.path}</span>{#if data.line != null}<span class="file-line">L{data.line}</span>{/if}</div>
			{:else if data.copied}
				<div class="file-row"><span class="file-op">已复制</span><span class="file-path">{data.from}</span></div>
				<div class="file-row"><span class="file-op-to">→</span><span class="file-path">{data.to}</span></div>
			{:else if data.moved}
				<div class="file-row"><span class="file-op">已移动</span><span class="file-path">{data.from}</span></div>
				<div class="file-row"><span class="file-op-to">→</span><span class="file-path">{data.to}</span></div>
			{:else if data.deleted}
				<div class="file-row"><span class="file-op">已删除</span><span class="file-path">{data.path}</span></div>
			{:else if Array.isArray(data.entries)}
				<div class="tool-card-count">{data.count ?? data.entries.length} 项</div>
				{#if data.entries.length > 0}
					<div class="tool-card-list">
						{#each data.entries as e (e)}
							<div class="env-row"><span class="env-name">{e}</span></div>
						{/each}
					</div>
				{:else}
					<p class="tool-card-empty">（空目录）</p>
				{/if}
			{:else}
				<div class="tool-card-meta">{data.size != null ? `${fmtBytes(data.size)} · ` : ''}读取完成</div>
				{#if typeof data.content === 'string' && data.content}
					<pre class="content-preview">{data.content}</pre>
				{/if}
			{/if}
		{:else if toolName === 'network'}
			<div class="job-row">
				<span class="status-badge status-{data.status >= 200 && data.status < 300 ? 'completed' : 'failed'}">{data.status}</span>
				{#if data.truncated}<span class="tool-card-meta">（响应过长已截断）</span>{/if}
			</div>
			{#if typeof data.body === 'string' && data.body}
				<pre class="content-preview">{data.body}</pre>
			{/if}
		{:else if toolName === 'clipboard'}
			{#if data.written}
				<p class="tool-card-empty">已写入剪贴板</p>
			{:else if typeof data.content === 'string' && data.content}
				<pre class="content-preview">{data.content}</pre>
			{:else}
				<p class="tool-card-empty">剪贴板为空</p>
			{/if}
		{:else if toolName === 'power'}
			{#if data.battery_percent != null}
				<div class="meter-row">
					<span class="meter-label">电池</span>
					<span class="meter-value">{data.battery_percent}%</span>
					<span class="meter-track"><span class="meter-fill" style="width: {clampPct(data.battery_percent)}%"></span></span>
					<span class="meter-sub">{data.battery_status ?? 'unknown'}{data.ac_power === 'online' ? ' · 已接电源' : ''}</span>
				</div>
			{:else}
				<p class="tool-card-empty">{data.locked ? '已锁定' : data.sleep ? '已休眠' : data.hibernate ? '已休眠' : '电源状态未知'}</p>
			{/if}
		{/if}

		{#if data.hint}
			<div class="tool-card-hint">{data.hint}</div>
		{/if}
	</div>
{/if}

<style>
	.tool-card {
		background: color-mix(in srgb, var(--md-sys-color-secondary-container) 45%, var(--md-sys-color-surface));
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-medium);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		min-width: 240px;
		max-width: 420px;
		margin-top: var(--md-sys-space-xs);
	}
	.tool-card-header {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		margin-bottom: var(--md-sys-space-xs);
	}
	.tool-card-icon {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 20px;
		height: 20px;
		border-radius: 50%;
		background: var(--md-sys-color-secondary);
		color: var(--md-sys-color-on-secondary);
		font-size: 12px;
		font-weight: 700;
		flex: none;
	}
	.tool-card-label {
		font-size: 12px;
		font-weight: 700;
		color: var(--md-sys-color-on-secondary-container);
	}
	.ask-question {
		margin: 0 0 var(--md-sys-space-sm);
		font-size: 13px;
		line-height: 1.5;
		color: var(--md-sys-color-on-surface);
	}
	.ask-options {
		display: flex;
		flex-wrap: wrap;
		gap: var(--md-sys-space-xs);
		margin-bottom: var(--md-sys-space-sm);
	}
	.ask-option {
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
		border: 1px solid var(--md-sys-color-outline-variant);
		border-radius: var(--md-sys-shape-full);
		padding: var(--md-sys-space-xs) var(--md-sys-space-md);
		font-size: 12px;
		font-weight: 600;
		cursor: pointer;
		transition: filter 0.15s ease;
	}
	.ask-option:hover {
		filter: brightness(0.95);
	}
	.ask-waiting {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		font-size: 12px;
		color: var(--md-sys-color-on-surface-variant);
	}
	.ask-waiting-dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--md-sys-color-secondary);
		animation: ask-pulse 1.2s ease-in-out infinite;
	}
	@keyframes ask-pulse {
		0%, 100% { opacity: 1; transform: scale(1); }
		50% { opacity: 0.35; transform: scale(0.8); }
	}
	.tool-card-count {
		font-size: 11px;
		font-weight: 600;
		color: var(--md-sys-color-on-surface-variant);
		margin-bottom: var(--md-sys-space-xs);
	}
	.tool-card-meta {
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		margin-top: var(--md-sys-space-2xs);
	}
	.tool-card-empty {
		margin: 0;
		font-size: 12px;
		color: var(--md-sys-color-on-surface-variant);
	}
	.tool-card-list {
		max-height: 200px;
		overflow-y: auto;
		border-radius: var(--md-sys-shape-extra-small);
	}
	.search-row,
	.window-row,
	.env-row,
	.file-row,
	.reminder-row {
		display: flex;
		align-items: baseline;
		gap: var(--md-sys-space-xs);
		padding: 3px var(--md-sys-space-2xs);
		border-radius: 4px;
		font-size: 12px;
	}
	.search-row:nth-child(odd),
	.window-row:nth-child(odd),
	.env-row:nth-child(odd),
	.reminder-row:nth-child(odd) {
		background: color-mix(in srgb, var(--md-sys-color-on-surface) 4%, transparent);
	}
	.search-path,
	.file-path,
	.env-name,
	.window-title,
	.reminder-title,
	.env-value {
		font-family: var(--md-sys-typescale-mono);
		font-size: 11px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.search-path {
		flex: 1;
		min-width: 0;
		color: var(--md-sys-color-on-surface);
	}
	.search-line {
		flex: none;
		font-size: 10px;
		font-weight: 700;
		color: var(--md-sys-color-secondary);
	}
	.search-snippet {
		flex: none;
		max-width: 140px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
	}
	.window-title {
		flex: 1;
		min-width: 0;
		color: var(--md-sys-color-on-surface);
	}
	.window-pid {
		flex: none;
		font-size: 10px;
		color: var(--md-sys-color-on-surface-variant);
	}
	.env-name {
		flex: none;
		font-weight: 600;
		color: var(--md-sys-color-secondary);
	}
	.env-value {
		flex: 1;
		min-width: 0;
		color: var(--md-sys-color-on-surface-variant);
	}
	.file-op {
		flex: none;
		font-size: 10px;
		font-weight: 700;
		padding: 1px 6px;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
	}
	.file-op-to {
		flex: none;
		font-size: 11px;
		color: var(--md-sys-color-on-surface-variant);
		width: 34px;
		text-align: center;
	}
	.file-path {
		flex: 1;
		min-width: 0;
		color: var(--md-sys-color-on-surface);
	}
	.file-line {
		flex: none;
		font-size: 10px;
		font-weight: 700;
		color: var(--md-sys-color-secondary);
	}
	.reminder-mode {
		flex: none;
		font-size: 10px;
		font-weight: 600;
		padding: 1px 6px;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-secondary-container);
		color: var(--md-sys-color-on-secondary-container);
	}
	.reminder-time {
		flex: none;
		font-size: 10px;
		color: var(--md-sys-color-on-surface-variant);
	}
	.proc-table {
		width: 100%;
		border-collapse: collapse;
		font-size: 11px;
	}
	.proc-table th {
		position: sticky;
		top: 0;
		background: color-mix(in srgb, var(--md-sys-color-secondary-container) 45%, var(--md-sys-color-surface));
		text-align: left;
		font-weight: 600;
		color: var(--md-sys-color-on-surface-variant);
		padding: 2px var(--md-sys-space-2xs);
		font-size: 10px;
	}
	.proc-table td {
		padding: 2px var(--md-sys-space-2xs);
		border-bottom: 1px solid color-mix(in srgb, var(--md-sys-color-on-surface) 6%, transparent);
	}
	.proc-name {
		max-width: 150px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--md-sys-color-on-surface);
	}
	.proc-num {
		text-align: right;
		font-family: var(--md-sys-typescale-mono);
		color: var(--md-sys-color-on-surface-variant);
	}
	.job-row {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		font-size: 12px;
	}
	.job-id {
		font-family: var(--md-sys-typescale-mono);
		font-size: 11px;
		color: var(--md-sys-color-on-surface);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.status-badge {
		flex: none;
		font-size: 10px;
		font-weight: 700;
		text-transform: uppercase;
		padding: 1px 8px;
		border-radius: var(--md-sys-shape-full);
	}
	.status-completed {
		background: var(--md-sys-color-success);
		color: var(--md-sys-color-on-success-container);
	}
	.status-failed {
		background: var(--md-sys-color-error);
		color: var(--md-sys-color-on-error);
	}
	.status-running {
		background: var(--md-sys-color-secondary);
		color: var(--md-sys-color-on-secondary);
	}
	.status-cancelled,
	.status-not_found {
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
	}
	.meter-row {
		display: grid;
		grid-template-columns: auto 1fr auto;
		align-items: center;
		column-gap: var(--md-sys-space-xs);
		margin-bottom: var(--md-sys-space-xs);
	}
	.meter-label {
		font-size: 11px;
		font-weight: 600;
		color: var(--md-sys-color-on-surface);
	}
	.meter-value {
		grid-column: 2;
		font-size: 11px;
		font-family: var(--md-sys-typescale-mono);
		color: var(--md-sys-color-on-surface-variant);
	}
	.meter-track {
		grid-column: 1 / -1;
		height: 6px;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-surface-container-high);
		overflow: hidden;
	}
	.meter-fill {
		display: block;
		height: 100%;
		border-radius: var(--md-sys-shape-full);
		background: var(--md-sys-color-secondary);
		transition: width 0.4s ease;
	}
	.meter-sub {
		grid-column: 2;
		font-size: 10px;
		color: var(--md-sys-color-on-surface-variant);
	}
	.sys-os {
		display: flex;
		align-items: center;
		gap: var(--md-sys-space-xs);
		margin-bottom: var(--md-sys-space-sm);
	}
	.sys-os-name {
		font-size: 12px;
		font-weight: 700;
		color: var(--md-sys-color-on-surface);
	}
	.sys-os-host {
		font-size: 11px;
		font-family: var(--md-sys-typescale-mono);
		color: var(--md-sys-color-on-surface-variant);
	}
	.content-preview {
		background: var(--md-sys-color-surface-container-high);
		color: var(--md-sys-color-on-surface-variant);
		padding: var(--md-sys-space-xs) var(--md-sys-space-sm);
		border-radius: var(--md-sys-shape-small);
		font-family: var(--md-sys-typescale-mono);
		font-size: 11px;
		white-space: pre-wrap;
		word-break: break-word;
		max-height: 180px;
		overflow-y: auto;
		margin: var(--md-sys-space-xs) 0 0;
	}
	.tool-card-hint {
		margin-top: var(--md-sys-space-xs);
		font-size: 10px;
		color: var(--md-sys-color-on-surface-variant);
		border-top: 1px dashed var(--md-sys-color-outline-variant);
		padding-top: var(--md-sys-space-xs);
	}
</style>
