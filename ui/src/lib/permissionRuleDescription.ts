export function describePermissionRuleImpact(key: string, effect: string): string {
	const normalizedKey = String(key || '').trim();
	if (!normalizedKey) return '规则会按 capability 层级匹配；拒绝优先于允许。';

	const match = `匹配“${normalizedKey}”本身及所有以“${normalizedKey}.”开头的下级操作或工具能力。`;
	return effect === 'deny'
		? `${match} 拒绝优先于任何匹配的允许规则。`
		: `${match} 任何匹配的拒绝规则仍会阻止操作。`;
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
