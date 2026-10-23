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
	return `访问 · ${origin(resource.scheme, resource.host, resource.port)}`;
}

export function permissionResourceScopeDescription(scope: PermissionResourceScope): string {
	if (scope.kind === 'local_path') {
		const extent = scope.recursive ? '目录及其子项' : '精确路径';
		const access = scope.access === 'read' ? '读取' : '写入';
		return `${access} · ${extent} · ${scope.path}`;
	}
	return `访问 · ${origin(scope.scheme, scope.host, scope.port)}`;
}
