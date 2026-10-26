import type { HookCommand, LifecycleHooksConfig } from '$lib/contracts/generatedCommands.ts';

export type HookListKey = keyof LifecycleHooksConfig;
export const MAX_HOOKS_PER_EVENT = 8;
export const HOOK_EVENTS: Array<{
	key: HookListKey;
	label: string;
	description: string;
	veto: boolean;
}> = [
	{
		key: 'pre_tool_use',
		label: 'PreToolUse',
		description: '工具执行前检查；可返回 allow 或 deny，任何失败都会阻止工具。',
		veto: true,
	},
	{
		key: 'post_tool_use',
		label: 'PostToolUse',
		description: '工具执行结束后通知；失败不改变工具结果。',
		veto: false,
	},
	{
		key: 'session_start',
		label: 'SessionStart',
		description: '当前进程中的会话首次执行时通知；重启后可再次触发。',
		veto: false,
	},
	{
		key: 'run_start',
		label: 'RunStart',
		description: '每次 Agent 执行开始时通知，包括恢复和继续执行。',
		veto: false,
	},
	{
		key: 'run_end',
		label: 'RunEnd',
		description: '执行完成、暂停、取消或出错时通知，包含 run_outcome。',
		veto: false,
	},
	{
		key: 'session_end',
		label: 'SessionEnd',
		description: '会话结束状态持久化后通知；删除会话不会触发。',
		veto: false,
	},
	{
		key: 'pre_compact',
		label: 'PreCompact',
		description: '上下文压缩前通知，包含 tokens_before。',
		veto: false,
	},
	{
		key: 'post_compact',
		label: 'PostCompact',
		description: '压缩结果提交成功后通知，包含压缩前后 token 估算。',
		veto: false,
	},
];

export const emptyHooks = (): LifecycleHooksConfig => ({
	pre_tool_use: [],
	post_tool_use: [],
	session_start: [],
	run_start: [],
	run_end: [],
	session_end: [],
	pre_compact: [],
	post_compact: [],
});

export const HOOK_TEMPLATES = {
	blank: { label: '空白命令', command: '', args: [] },
	powershell: {
		label: 'PowerShell 脚本',
		command: 'pwsh.exe',
		args: ['-NoProfile', '-NonInteractive', '-File', 'scripts/hook.ps1'],
	},
	python: { label: 'Python 脚本', command: 'python.exe', args: ['scripts/hook.py'] },
	node: { label: 'Node.js 脚本', command: 'node.exe', args: ['scripts/hook.mjs'] },
} satisfies Record<string, { label: string; command: string; args: string[] }>;
export type HookTemplateKey = keyof typeof HOOK_TEMPLATES;

export function createHook(template: HookTemplateKey = 'blank'): HookCommand {
	const source = HOOK_TEMPLATES[template];
	return { enabled: true, command: source.command, args: [...source.args], matcher: null };
}

const byteLength = (value: string) => new TextEncoder().encode(value).length;
// Rust char::is_control covers the Unicode Cc category.
const hasControl = (value: string) => /\p{Cc}/u.test(value);

export function hookValidationErrors(hook: HookCommand): string[] {
	const errors: string[] = [];
	if (!hook.command.trim() || byteLength(hook.command) > 2048 || hasControl(hook.command))
		errors.push('可执行文件不能为空，不能含控制字符，且不能超过 2048 UTF-8 字节。');
	if (hook.args.length > 32) errors.push('参数最多 32 项。');
	if (hook.args.some((arg) => byteLength(arg) > 4096 || hasControl(arg)))
		errors.push('每项参数不能含控制字符，且不能超过 4096 UTF-8 字节。');
	if (
		hook.matcher !== null &&
		(!hook.matcher ||
			byteLength(hook.matcher) > 128 ||
			/[\p{Cc}\p{White_Space}]/u.test(hook.matcher))
	)
		errors.push(
			'matcher 不能为空或含空白、控制字符，且不能超过 128 UTF-8 字节；仅 * 为通配符。',
		);
	return errors;
}

/** Local preview only; runtime matching and authorization remain in Rust. */
export function hookMatches(hook: HookCommand, toolName: string | null): boolean {
	if (!hook.enabled) return false;
	if (hook.matcher === null || hook.matcher === '*') return true;
	if (toolName === null) return false;
	// Keep only the latest star as a retry point. Regex translation can have
	// exponential backtracking even for a valid 128-byte matcher.
	const pattern = [...hook.matcher];
	const value = [...toolName];
	let patternIndex = 0;
	let valueIndex = 0;
	let starIndex = -1;
	let retryIndex = 0;
	while (valueIndex < value.length) {
		if (pattern[patternIndex] !== '*' && pattern[patternIndex] === value[valueIndex]) {
			patternIndex++;
			valueIndex++;
		} else if (pattern[patternIndex] === '*') {
			starIndex = patternIndex++;
			retryIndex = valueIndex;
		} else if (starIndex >= 0) {
			valueIndex = ++retryIndex;
			patternIndex = starIndex + 1;
		} else return false;
	}
	return pattern.slice(patternIndex).every((character) => character === '*');
}

/** Strict, bounded import into the unsaved draft, never into runtime config. */
export function parseHooksImport(text: string): LifecycleHooksConfig {
	if (text.length > 8 * 1024 * 1024 || byteLength(text) > 8 * 1024 * 1024)
		throw new Error('配置不能超过 8 MiB。');
	let input: unknown;
	try {
		input = JSON.parse(text);
	} catch {
		throw new Error('请输入有效的 Hook JSON 配置。');
	}
	if (!input || typeof input !== 'object' || Array.isArray(input))
		throw new Error('配置必须是事件列表对象。');
	const output = emptyHooks();
	for (const [key, list] of Object.entries(input)) {
		if (!Object.hasOwn(output, key)) throw new Error('配置包含未知事件。');
		if (!Array.isArray(list) || list.length > MAX_HOOKS_PER_EVENT)
			throw new Error('每个事件必须是最多 8 条命令的数组。');
		output[key as HookListKey] = list.map((value: unknown) => {
			if (!value || typeof value !== 'object' || Array.isArray(value))
				throw new Error('Hook 必须是命令对象。');
			const item = value as Record<string, unknown>;
			if (
				Object.keys(item).some(
					(field) => !['enabled', 'command', 'args', 'matcher'].includes(field),
				) ||
				typeof item.command !== 'string' ||
				(item.enabled !== undefined && typeof item.enabled !== 'boolean') ||
				(item.args !== undefined &&
					(!Array.isArray(item.args) ||
						!item.args.every((arg) => typeof arg === 'string'))) ||
				(item.matcher !== undefined &&
					item.matcher !== null &&
					typeof item.matcher !== 'string')
			)
				throw new Error('Hook 字段类型错误或包含未知字段。');
			const hook: HookCommand = {
				enabled: item.enabled ?? true,
				command: item.command,
				args: item.args ?? [],
				matcher: item.matcher ?? null,
			} as HookCommand;
			const errors = hookValidationErrors(hook);
			if (errors.length) throw new Error(errors[0]);
			return hook;
		});
	}
	return output;
}

export function hookFailureHelp(code: string | null): string {
	switch (code) {
		case 'security_policy_denied':
			return '检查执行边界：FullAccess 需开放网络且无域名白名单；WorkspaceWrite 需 Windows MXC 且 Hook 路径须在授权范围内。';
		case 'policy_changed':
			return '执行期间安全策略已变更，进程树已终止。';
		case 'spawn_failed':
			return '检查可执行文件是否已安装、路径是否正确，以及脚本文件是否存在。';
		case 'nonzero_exit':
			return '脚本退出码非零；请在本地使用示例 stdin 检查脚本。';
		case 'invalid_pre_tool_reply':
			return 'PreToolUse 的 stdout 只能为空或包含 allow/deny JSON；调试信息请写 stderr。';
		case 'time_limit':
		case 'event_time_limit':
			return '单事件总预算为 5 秒；缩短脚本耗时或减少同事件的命令。';
		case 'output_limit':
			return '每个 stdout/stderr 流最多 64 KiB；请减少输出。';
		case 'payload_exceeds_limit':
			return '事件输入超过 64 KiB；PreToolUse 为安全起见拒绝执行。';
		case 'project_configuration_invalid':
			return '检查已信任项目的 .haven/hooks.toml 格式、大小和重解析点。';
		case 'execution_workspace_unavailable':
		case 'execution_workspace_changed':
			return '检查当前执行工作区是否可用，待工作区切换完成后重试。';
		case 'configuration_unavailable':
			return '配置快照不可用；请重新读取设置后重试。';
		case 'cancelled':
			return '本次 Hook 已随主操作取消。';
		case 'io_failed':
			return '脚本输入输出未能完成；检查脚本是否完整读取 stdin 并正常退出。';
		case 'too_many_matching_commands':
			return '用户级与项目级合计每事件最多匹配 16 条命令。';
		default:
			return code ? '请检查本地脚本和当前安全策略。' : '';
	}
}
