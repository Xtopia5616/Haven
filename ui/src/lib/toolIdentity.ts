/** Shared tool-name → source / label helpers for chat tool cards. */

import {
	getToolManifest,
	toolLabel,
	toolRepresentedSource,
	toolRootName,
} from './toolManifest.ts';

export type ToolSource = 'builtin' | 'skill' | 'mcp';

/** @type {Record<string, string>} */
export const TOOL_LABELS: Record<string, string> = {
	files: '文件与搜索',
	media: '媒体',
	http: 'HTTP 请求',
	system: '系统与桌面',
	shell: '终端输出',
	notify: '通知',
	haven: 'Haven 管理与会话工具',
	memory: '记忆',
	load_mcp: '加载 MCP',
	// Skills are registered directly as independent tools.
	web_search: '联网搜索',
	agent: 'Agent 协作',
	process: '进程',
	clipboard: '剪贴板',
	input: '输入控制',
	window: '窗口与屏幕',
	preferences: '会话偏好',
	checklist: '检查清单',
	actions: '后台任务',
	schedule: '定时任务',
	haven_diagnostics: 'Haven 诊断',
	haven_config: 'Haven 配置',
	haven_skills: 'Haven 技能',
	haven_tools: '内置工具管理',
	haven_mcp: 'MCP 管理',
};

export { toolRootName };

/** Return the fixed operation for a model-facing operation view. */
export function toolOperationName(toolName: string): string | null {
	const manifest = getToolManifest(toolName);
	return manifest?.identity.operation ?? null;
}

/** Strip the provider-safe namespace prefix from a dynamic tool name. */
function stripToolPrefix(name: string, prefix: string): string | null {
	if (!name.startsWith(prefix)) return null;
	const rest = name.slice(prefix.length);
	return rest || name;
}

/** Classify a wire tool name into builtin / skill / MCP. */
export function classifyToolSource(toolName: string): ToolSource {
	const name = String(toolName || '');
	const representedSource = toolRepresentedSource(name);
	if (representedSource === 'mcp') return 'mcp';
	if (representedSource === 'skill') return 'skill';
	if (representedSource === 'builtin') return 'builtin';
	// The activation tools are implemented by Haven, but their card represents
	// the capability being activated, so keep them visually consistent with the
	// dynamic tools they expose.
	if (name === 'load_mcp' || name.startsWith('mcp__')) {
		return 'mcp';
	}
	if (name.startsWith('skill__')) {
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
	const manifestLabel = toolLabel(name);
	if (manifestLabel) return manifestLabel;
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
