export function describePermissionRuleImpact(key: string, effect: string): string {
	const normalizedKey = String(key || '').trim();
	if (!normalizedKey) return '规则会按 capability 层级匹配；明确资源授权优先。';

	const match = `匹配“${normalizedKey}”本身及所有以“${normalizedKey}.”开头的下级操作或工具能力。`;
	return effect === 'deny'
		? `${match} 明确资源授权优先于匹配的拒绝规则；未授权资源不会执行。`
		: `${match} 明确资源授权优先于匹配的拒绝规则。`;
}

export function permissionTargetLabel(target: string): string {
	switch (target) {
		case 'operation':
			return '单项操作';
		case 'group':
			return '功能组';
		case 'tool':
			return '工具';
		default:
			return '未知层级';
	}
}
