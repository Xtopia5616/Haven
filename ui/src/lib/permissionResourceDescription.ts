import type {
	DeclaredResource,
	PermissionResourceScope,
} from '$lib/contracts/generatedCommands.ts';

function origin(scheme: string, host: string, port: number | null | undefined): string {
	return `${scheme}://${host}${port == null ? '' : `:${port}`}`;
}

export function declaredResourceDescription(resource: DeclaredResource): string {
	if (resource.kind === 'local_path') {
		return `${resource.access === 'read' ? '读取' : '写入'} · ${resource.path}`;
	}
	if (resource.kind === 'network_destination') {
		return `访问 · ${origin(resource.scheme, resource.host, resource.port)}`;
	}
	if (resource.kind === 'clipboard') {
		return resource.access === 'read' ? '读取系统剪贴板' : '写入系统剪贴板';
	}
	return `读取受管媒体资产 · ${resource.asset_id}`;
}

export function permissionResourceScopeDescription(scope: PermissionResourceScope): string {
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
	return `读取受管媒体资产 · ${scope.asset_id}`;
}
