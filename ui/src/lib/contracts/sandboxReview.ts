import type { SandboxOutputReviewManifestDto } from './generatedCommands.ts';
import { isRecord } from './objectGuards.ts';

const SHA256 = /^[0-9a-f]{64}$/;
const CHANGE_KINDS = new Set(['created', 'modified', 'deleted', 'type_changed']);

function safeCount(value: unknown, maximum = Number.MAX_SAFE_INTEGER): value is number {
	return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 && value <= maximum;
}

function safeHash(value: unknown): value is string | null {
	return value === null || (typeof value === 'string' && SHA256.test(value));
}

function safeRelativePath(value: unknown): value is string {
	if (
		typeof value !== 'string' ||
		value.length === 0 ||
		value.length > 1024 ||
		value.startsWith('/') ||
		value.startsWith('\\') ||
		/^[a-zA-Z]:/.test(value) ||
		/[\u0000-\u001f\u007f-\u009f]/u.test(value)
	) {
		return false;
	}
	return !value
		.replaceAll('\\', '/')
		.split('/')
		.some((part) => part === '..' || part === '.' || part === '');
}

/** Validate the renderer projection without ever accepting source contents or absolute paths. */
export function parseSandboxOutputReview(
	value: unknown,
): SandboxOutputReviewManifestDto | null {
	if (value == null) return null;
	if (!isRecord(value)) throw new Error('Invalid sandbox output review');
	if (
		typeof value.sessionId !== 'string' ||
		!/^ses-[0-9a-f]{32}$/.test(value.sessionId) ||
		typeof value.sourceStepId !== 'string' ||
		!/^step-[0-9a-f]{32}$/.test(value.sourceStepId) ||
		!safeCount(value.expiresAtUnixMs) ||
		!safeCount(value.storageChargeBytes, 512 * 1024 * 1024) ||
		!Array.isArray(value.changes) ||
		value.changes.length > 4096
	) {
		throw new Error('Invalid sandbox output review');
	}

	const indices = new Set<number>();
	for (const change of value.changes) {
		if (
			!isRecord(change) ||
			!safeCount(change.changeIndex, 4095) ||
			indices.has(change.changeIndex) ||
			!safeRelativePath(change.relativePath) ||
			typeof change.change !== 'string' ||
			!CHANGE_KINDS.has(change.change) ||
			!(change.beforeBytes == null || safeCount(change.beforeBytes)) ||
			!(change.afterBytes == null || safeCount(change.afterBytes)) ||
			!safeHash(change.beforeSha256) ||
			!safeHash(change.afterSha256) ||
			typeof change.promoted !== 'boolean'
		) {
			throw new Error('Invalid sandbox output review change');
		}
		indices.add(change.changeIndex);
	}

	return value as unknown as SandboxOutputReviewManifestDto;
}
