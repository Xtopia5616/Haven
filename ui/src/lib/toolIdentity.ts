/** Shared tool-name → source / label helpers for chat tool cards. */

export type ToolSource = 'builtin' | 'skill' | 'mcp';

/** @type {Record<string, string>} */
export const TOOL_LABELS: Record<string, string> = {
	files: '文件与搜索',
	process: '进程列表',
	window: '窗口列表',
	actions: '后台任务',
	schedule: '定时任务',
	http: 'HTTP 请求',
	clipboard: '剪贴板',
	system: '系统',
	shell: '终端输出',
	notify: '通知',
	audio: '音频',
	input: '输入操作',
	haven_config: 'Haven 配置',
	haven_diagnostics: 'Haven 诊断',
	haven_mcp: 'Haven MCP',
	haven_skills: 'Haven 技能',
	haven_tools: 'Haven 工具',
	memory: '记忆',
	load_mcp: '加载 MCP',
	load_skill: '加载技能',
	web_search: '联网搜索',
	agent: 'Agent 协作',
};

/** Strip the provider-safe namespace prefix from a dynamic tool name. */
function stripToolPrefix(name: string, prefix: string): string | null {
	if (!name.startsWith(prefix)) return null;
	const rest = name.slice(prefix.length);
	return rest || name;
}

/** Classify a wire tool name into builtin / skill / MCP. */
export function classifyToolSource(toolName: string): ToolSource {
	const name = String(toolName || '');
	// The activation tools are implemented by Haven, but their card represents
	// the capability being activated, so keep them visually consistent with the
	// dynamic tools they expose.
	if (name === 'load_mcp' || name.startsWith('mcp__')) {
		return 'mcp';
	}
	if (name === 'load_skill' || name.startsWith('skill__')) {
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
 * MCP/Skill strip their provider-safe wire prefix (`mcp__` / `skill__`).
 */
export function toolDisplayName(
	toolName: string,
	labels: Record<string, string> = TOOL_LABELS,
): string {
	const name = String(toolName || '');
	if (labels[name]) return labels[name];
	const stripped = stripToolPrefix(name, 'mcp__') ?? stripToolPrefix(name, 'skill__');
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
