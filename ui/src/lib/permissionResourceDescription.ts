import type {
	DeclaredResource,
	DesktopAccess,
	PermissionResourceScope,
} from '$lib/contracts/generatedCommands.ts';

function origin(scheme: string, host: string, port: number | null | undefined): string {
	return `${scheme}://${host}${port == null ? '' : `:${port}`}`;
}

function desktopAccessDescription(access: DesktopAccess): string {
	if (access === 'read') return '读取桌面窗口信息';
	if (access === 'capture') return '截取桌面画面';
	return '子进程共享交互桌面，可访问窗口（剪贴板和输入注入仍受限制）';
}

export function declaredResourceDescription(resource: DeclaredResource): string {
	if (resource.kind === 'process_network')
		return '子进程网络 · 公网、私网双向通信与宿主回环（目标域名无法逐个约束）';
	if (resource.kind === 'local_path') {
		return `${resource.access === 'read' ? '读取' : '写入'} · ${resource.path}`;
	}
	if (resource.kind === 'network_destination') {
		return `访问 · ${origin(resource.scheme, resource.host, resource.port)}`;
	}
	if (resource.kind === 'clipboard') {
		return resource.access === 'read' ? '读取系统剪贴板' : '写入系统剪贴板';
	}
	if (resource.kind === 'desktop') {
		return desktopAccessDescription(resource.access);
	}
	if (resource.kind === 'window') {
		const access =
			resource.access === 'read'
				? '读取窗口'
				: resource.access === 'interact'
					? '操作窗口'
					: '关闭窗口';
		return `${access} · ${resource.window_id} · PID ${resource.process_id}${resource.element_fingerprint ? ' · 指定控件' : ''}`;
	}
	if (resource.kind === 'registry') {
		const location = `${resource.hive}${resource.path ? `\\${resource.path}` : ''}`;
		return resource.access === 'enumerate_names'
			? `列举注册表名称 · ${location}`
			: `读取注册表值 · ${location} · ${resource.value_name ?? ''}`;
	}
	if (resource.kind === 'mcp_remote_invocation') {
		return 'MCP 远程调用 · 已绑定连接与精确参数';
	}
	return `读取受管媒体资产 · ${resource.asset_id}`;
}

export function permissionResourceScopeDescription(scope: PermissionResourceScope): string {
	if (scope.kind === 'process_network')
		return '指定子进程调用的网络 · 公网、私网双向通信与宿主回环';
	if (scope.kind === 'local_path') {
		const extent = scope.recursive ? '目录及其子项' : '精确路径';
		const access = scope.access === 'read' ? '读取' : '写入';
		return `${access} · ${extent} · ${scope.path}`;
	}
	if (scope.kind === 'network_destination') {
		return `访问 · ${origin(scope.scheme, scope.host, scope.port)}`;
	}
	if (scope.kind === 'clipboard') {
		return scope.access === 'read' ? '读取系统剪贴板' : '写入系统剪贴板';
	}
	if (scope.kind === 'desktop') {
		return desktopAccessDescription(scope.access);
	}
	if (scope.kind === 'window') {
		const access =
			scope.access === 'read'
				? '读取窗口'
				: scope.access === 'interact'
					? '操作窗口'
					: '关闭窗口';
		return `${access} · ${scope.window_id} · PID ${scope.process_id}${scope.element_fingerprint ? ' · 指定控件' : ''}`;
	}
	if (scope.kind === 'registry') {
		const location = `${scope.hive}${scope.path ? `\\${scope.path}` : ''}`;
		return scope.access === 'enumerate_names'
			? `列举注册表名称 · ${location}`
			: `读取注册表值 · ${location} · ${scope.value_name ?? ''}`;
	}
	if (scope.kind === 'mcp_remote_invocation') {
		return 'MCP 远程调用 · 已绑定连接与精确参数';
	}
	return `读取受管媒体资产 · ${scope.asset_id}`;
}
