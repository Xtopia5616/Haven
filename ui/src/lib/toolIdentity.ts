/** Shared tool-name → source / label helpers for chat tool cards. */

export type ToolSource = 'builtin' | 'skill' | 'mcp';

/** @type {Record<string, string>} */
export const TOOL_LABELS: Record<string, string> = {
	search: '文件搜索',
	file_search: '文件搜索',
	process: '进程列表',
	window: '窗口列表',
	actions: '后台任务',
	schedule: '定时任务',
	file: '文件操作',
	files: '文件与搜索',
	http: 'HTTP 请求',
	clipboard: '剪贴板',
	system: '系统',
	shell: '终端输出',
	notify: '通知',
	audio: '音频',
	input: '输入操作',
	haven: 'Haven 自身',
	memory: '记忆',
	load_mcp: '加载 MCP',
	load_skill: '加载技能',
	web_search: '联网搜索',
	agent: 'Agent 协作',
};

/**
 * Wire tool names from `llm_tool_name`: `mcp::server::tool` / `skill::name`
 * become `mcp__server__tool` / `skill__name`. Older UI fixtures may still use
 * a single underscore or the raw `::` form.
 */
function stripToolPrefix(name: string, prefixes: string[]): string | null {
	for (const prefix of prefixes) {
		if (name.startsWith(prefix)) {
			const rest = name.slice(prefix.length);
			return rest || name;
		}
	}
	return null;
}

/** Classify a wire tool name into builtin / skill / MCP. */
export function classifyToolSource(toolName: string): ToolSource {
	const name = String(toolName || '');
	// Check double-underscore wire form first; `mcp__` also starts with `mcp_`.
	if (name.startsWith('mcp__') || name.startsWith('mcp_') || name.startsWith('mcp::')) {
		return 'mcp';
	}
	if (name.startsWith('skill__') || name.startsWith('skill_') || name.startsWith('skill::')) {
		return 'skill';
	}
	return 'builtin';
}

/** Short Chinese/English badge text for the tool source. */
export function toolSourceLabel(source: ToolSource): string {
	switch (source) {
		case 'mcp':
			return 'MCP';
		case 'skill':
			return 'Skill';
		default:
			return '内置';
	}
}

/**
 * Human-readable tool title for the card header. Builtins use Chinese labels;
 * MCP/Skill strip their wire prefix (`mcp__` / `skill__`, plus legacy forms).
 * `__` must be checked before `_` because `mcp__` also starts with `mcp_`.
 */
export function toolDisplayName(
	toolName: string,
	labels: Record<string, string> = TOOL_LABELS,
): string {
	const name = String(toolName || '');
	if (labels[name]) return labels[name];
	const stripped =
		stripToolPrefix(name, ['mcp__', 'mcp::', 'mcp_']) ??
		stripToolPrefix(name, ['skill__', 'skill::', 'skill_']);
	return stripped ?? name;
}

/**
 * Normalize live `input` (object) or resume `action_input` (JSON string) into
 * a JSON-viewable value. Returns null when nothing useful is present.
 */
export function parseToolArgs(toolArgs: unknown): unknown | null {
	if (toolArgs == null) return null;
	if (typeof toolArgs === 'string') {
		const trimmed = toolArgs.trim();
		if (!trimmed) return null;
		try {
			return JSON.parse(trimmed);
		} catch {
			return { raw: trimmed };
		}
	}
	if (typeof toolArgs === 'object') return toolArgs;
	return { value: toolArgs };
}
