import type { ShellExecutionSecurityStatus } from '$lib/contracts/shellExecution.ts';

const NETWORK_POLICY_TEXT = {
	deny: ['禁止网络', 'Haven 拒绝声明需要网络的工具；这不是 Windows 防火墙规则，不会限制其它程序或已经运行的进程。'],
	ask: [
		'请求确认',
		'可验证的公网请求先询问；工作区可写模式会拦截无法验证目的地的 MCP、技能和命令子进程。完整访问模式下，经确认启动的子进程仍按当前 Windows 用户权限联网。',
	],
	restricted: [
		'受限网络',
		'只允许 Haven 能验证目的地的公共网络请求；无法按目的地检查的 MCP、技能和命令子进程会被拒绝。这不会限制其它程序或已运行进程的系统网络权限。',
	],
	open: [
		'开放网络',
		'Haven 不添加全局网络封锁或确认，工具自身授权仍会执行；获准启动的子进程按当前 Windows 用户权限联网。',
	],
} as const;

function gateStatusText(status: ShellExecutionSecurityStatus['brokerGateStatus']): string {
	switch (status) {
		case 'attested':
			return 'broker 正在运行，启动配额探测已通过';
		case 'quota_unattested':
			return 'broker 未能证明存储配额，Workspace Write Shell 会被拒绝';
		case 'service_unavailable':
			return 'broker 未安装或未运行，Workspace Write Shell 会被拒绝';
		case 'unsupported_platform':
			return '当前平台不支持 HCS Workspace Write Shell';
		case 'unknown':
		default:
			return '无法确认 broker 状态，Workspace Write Shell 会被拒绝';
	}
}

/** Renderer-safe explanation of the active runtime security policy. */
export function describeShellExecutionStatus(
	status: ShellExecutionSecurityStatus | null | undefined,
): { title: string; details: string; networkTitle: string; networkDetails: string } {
	if (!status) {
		return {
			title: '执行边界状态不可用',
			details: '尚未读取当前生效策略；无法读取的状态不能视为隔离已就绪。后端仍会在执行前复核授权。',
			networkTitle: '网络策略未知',
			networkDetails: '请刷新状态后再批准 Shell 操作。',
		};
	}

	const [networkTitle, networkDetails] = NETWORK_POLICY_TEXT[status.networkPolicy];
	switch (status.sandboxMode) {
		case 'read_only':
			return {
				title: '只读模式会拒绝 Shell',
				details: '此模式不启动本机 Shell 或 HCS guest。',
				networkTitle: `网络策略：${networkTitle}`,
				networkDetails,
			};
		case 'workspace_write':
			return {
				title:
					status.brokerGateStatus === 'attested'
						? 'Windows HCS guest · 配额启动探测已通过'
						: `Windows HCS guest · ${gateStatusText(status.brokerGateStatus)}`,
				details:
					'命令使用执行工作区的受限副本，工作目录限定在副本内；结果进入审阅，需逐项晋升。HCS guest 不配置网络端点。运行状态不代替目标 Windows 环境的完整验收。',
				networkTitle: `网络策略：${networkTitle}`,
				networkDetails,
			};
		case 'full_access':
		default:
			return {
				title: '本机 Shell · 当前 Windows 用户权限',
				details:
					'命令作为 Haven 子进程在本机运行。执行工作区只决定默认工作目录，不限制命令访问该用户有权访问的文件或网络。',
				networkTitle: `网络策略：${networkTitle}`,
				networkDetails,
			};
	}
}
