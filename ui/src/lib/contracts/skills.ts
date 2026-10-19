/** Skills command response contracts at the Rust-to-renderer boundary. */
import type {
	SkillExecutionResponse as GeneratedSkillExecutionResponse,
	SkillInfo as GeneratedSkillInfo,
} from './generatedCommands.ts';
import { isRecord } from './objectGuards.ts';
import { hasOwnWireField, isCanonicalEntityId } from './wireGuards.ts';
import { isBoolean, isNonEmptyString, isString } from './valueGuards.ts';

export interface SkillInfoView {
	name: string;
	description: string;
	version: string | null;
	language: string;
	enabled: boolean;
	executable: boolean;
	hasScript: boolean;
	unavailableReason: string | null;
	manifestError: string | null;
}

interface ValidatedSkillInfo extends GeneratedSkillInfo {}

function isNullableString(value: unknown): value is string | null {
	return value === null || isString(value);
}

function isSkillInfo(value: unknown): value is ValidatedSkillInfo {
	return (
		isRecord(value) &&
		isNonEmptyString(value.name) &&
		isString(value.description) &&
		isNullableString(value.version) &&
		isString(value.language) &&
		isBoolean(value.enabled) &&
		isBoolean(value.executable) &&
		isBoolean(value.has_script) &&
		isNullableString(value.unavailable_reason) &&
		isNullableString(value.manifest_error)
	);
}

function toSkillInfoView(skill: ValidatedSkillInfo): SkillInfoView {
	return {
		name: skill.name,
		description: skill.description,
		version: skill.version,
		language: skill.language,
		enabled: skill.enabled,
		executable: skill.executable,
		hasScript: skill.has_script,
		unavailableReason: skill.unavailable_reason,
		manifestError: skill.manifest_error,
	};
}

/** Validate complete Skill rows and map Rust wire fields to renderer camelCase. */
export function parseSkillInfoList(value: unknown): SkillInfoView[] {
	if (!Array.isArray(value)) throw new Error('Invalid Skills list response');

	const names = new Set<string>();
	return value.map((skill: unknown) => {
		if (!isSkillInfo(skill)) throw new Error('Invalid Skill info');
		if (names.has(skill.name)) throw new Error('Duplicate Skill name');
		names.add(skill.name);
		return toSkillInfoView(skill);
	});
}

/** Validate the fixed execution envelope while leaving its JSON output dynamic. */
export function parseSkillExecutionResponse(value: unknown): GeneratedSkillExecutionResponse {
	if (
		!isRecord(value) ||
		!isBoolean(value.success) ||
		!hasOwnWireField(value, 'output') ||
		!isNullableString(value.error) ||
		!(value.stepId === null || isCanonicalEntityId(value.stepId, 'step'))
	) {
		throw new Error('Invalid Skill execution response');
	}

	return {
		success: value.success,
		output: value.output,
		error: value.error,
		stepId: value.stepId,
	};
}
