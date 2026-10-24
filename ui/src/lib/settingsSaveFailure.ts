import { formatError } from './formatError.ts';

const PARTIAL_APPLY_ERROR_PREFIX = '部分 apply 失败：';

export type SettingsFailureTone = 'error' | 'warning';

export interface SettingsFailurePresentation {
	tone: SettingsFailureTone;
	title: string;
	message: string;
	detail: string;
}

export function isPartialConfigApplyError(error: unknown): boolean {
	return formatError(error).startsWith(PARTIAL_APPLY_ERROR_PREFIX);
}

export function presentSettingsFailure(
	error: unknown,
	operation: 'save' | 'reapply' = 'save',
): SettingsFailurePresentation {
	const detail = formatError(error);

	if (isPartialConfigApplyError(error)) {
		return {
			tone: 'warning',
			title: '设置已保存，但部分功能尚未生效',
			message:
				'配置已经写入。可点击“重试已保存配置”，也可以重启应用，让功能按已保存配置重新初始化。',
			detail,
		};
	}

	if (
		/credential reference (?:does not belong|is assigned|cannot be assigned)|assigned to multiple/i.test(
			detail,
		)
	) {
		return {
			tone: 'error',
			title: '凭据与配置项不匹配',
			message:
				'请在对应的服务商、模型或 MCP 环境变量中重新填写凭据后保存；同一凭据不能分配给多个配置项。',
			detail,
		};
	}

	if (/credential values must be staged/i.test(detail)) {
		return {
			tone: 'error',
			title: '凭据尚未安全保存',
			message: '请在对应配置项重新输入凭据并保存，应用会先将凭据写入 Windows 凭据管理器。',
			detail,
		};
	}

	if (/missing from secure storage|has no secure storage reference/i.test(detail)) {
		return {
			tone: 'error',
			title: '找不到已保存的凭据',
			message:
				'当前配置引用的凭据不可用。请在对应配置项重新填写凭据并保存；如果反复发生，可展开技术诊断查看原因。',
			detail,
		};
	}

	if (
		/failed to (?:write|read|update|remove).*credential|credential.*(?:secure storage|securely)|secure storage.*credential|credentials could not be saved/i.test(
			detail,
		)
	) {
		return {
			tone: 'error',
			title: '无法访问系统凭据存储',
			message:
				'凭据没有保存成功。请稍后重试；如果仍然失败，请检查 Windows 凭据管理器是否可用，再重新填写凭据。',
			detail,
		};
	}

	if (operation === 'reapply') {
		return {
			tone: 'error',
			title: '重新应用配置失败',
			message: '已保存的配置未能完整应用。可以稍后重试；技术诊断可在下方展开查看。',
			detail,
		};
	}

	return {
		tone: 'error',
		title: '设置未能保存',
		message: '请检查设置内容后重试；如果问题持续发生，可展开技术诊断查看原因。',
		detail,
	};
}
