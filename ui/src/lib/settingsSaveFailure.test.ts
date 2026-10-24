import { describe, expect, it } from 'vitest';
import { isPartialConfigApplyError, presentSettingsFailure } from './settingsSaveFailure.ts';

describe('settingsSaveFailure', () => {
	it('recognizes a config write followed by a failed runtime apply', () => {
		expect(
			isPartialConfigApplyError(
				'部分 apply 失败：配置已写入；重启应用后会从配置重新初始化。skills failed',
			),
		).toBe(true);
		expect(
			presentSettingsFailure(
				'部分 apply 失败：配置已写入；重启应用后会从配置重新初始化。skills failed',
			),
		).toMatchObject({ tone: 'warning', title: '设置已保存，但部分功能尚未生效' });
	});

	it('keeps ordinary save failures on the existing error path', () => {
		expect(isPartialConfigApplyError('保存配置失败：disk full')).toBe(false);
	});

	it('turns credential ownership errors into actionable Chinese copy and keeps diagnostics', () => {
		const failure = presentSettingsFailure(
			'provider credential reference does not belong to this configuration',
		);

		expect(failure.tone).toBe('error');
		expect(failure.title).toBe('凭据与配置项不匹配');
		expect(failure.message).toContain('重新填写凭据');
		expect(failure.detail).toContain('provider credential reference');
	});

	it('keeps sensitive values out of the expandable diagnostic detail', () => {
		const failure = presentSettingsFailure(
			'failed to write credential to secure storage: api_key=sk-1234567890abcdef',
		);

		expect(failure.title).toBe('无法访问系统凭据存储');
		expect(failure.detail).not.toContain('sk-1234567890abcdef');
		expect(failure.detail).toContain('[REDACTED]');
	});

	it('uses a distinct title and message for runtime reapply failures', () => {
		expect(presentSettingsFailure('runtime target unavailable', 'reapply')).toMatchObject({
			tone: 'error',
			title: '重新应用配置失败',
		});
	});
});
