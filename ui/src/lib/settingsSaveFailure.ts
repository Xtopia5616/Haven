import { formatError } from './formatError.ts';

const PARTIAL_APPLY_ERROR_PREFIX = '部分 apply 失败：';

export const PARTIAL_APPLY_SAVE_MESSAGE =
	'设置已保存，但部分运行时未应用；重启会从磁盘配置重新初始化。';

export function isPartialConfigApplyError(error: unknown): boolean {
	return formatError(error).startsWith(PARTIAL_APPLY_ERROR_PREFIX);
}
