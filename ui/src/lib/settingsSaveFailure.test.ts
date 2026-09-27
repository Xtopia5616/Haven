import { describe, expect, it } from 'vitest';
import { isPartialConfigApplyError, PARTIAL_APPLY_SAVE_MESSAGE } from './settingsSaveFailure.ts';

describe('settingsSaveFailure', () => {
	it('recognizes a durable config save followed by a failed runtime apply', () => {
		expect(
			isPartialConfigApplyError(
				'部分 apply 失败：配置已保存；重启应用后会从磁盘配置重新初始化。skills failed',
			),
		).toBe(true);
		expect(PARTIAL_APPLY_SAVE_MESSAGE).toContain('设置已保存');
		expect(PARTIAL_APPLY_SAVE_MESSAGE).toContain('重启会从磁盘配置重新初始化');
	});

	it('keeps ordinary save failures on the existing error path', () => {
		expect(isPartialConfigApplyError('保存配置失败：disk full')).toBe(false);
	});
});
