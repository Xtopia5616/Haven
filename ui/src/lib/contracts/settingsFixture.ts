import { SETTINGS_RESPONSE_SHAPE } from './generatedCommands.ts';
import type { SettingsPayload } from './settings.ts';
import type { GeneratedWireShape } from './generatedWireShape.ts';

const SETTINGS_NUMERIC_FIXTURE_VALUES: Record<string, number> = {
	'settings.context_limits.compaction_reserve_tokens': 8_192,
	'settings.context_limits.default_context_window': 64_000,
	'settings.context_limits.response_token_floor': 32_000,
	'settings.media.audio.max_duration_secs': 60,
	'settings.media.audio.silence_timeout_ms': 1_500,
	'settings.media.audio.vad_threshold': 0.5,
	'settings.media.stt.timeout_secs': 30,
	'settings.media.ocr.timeout_secs': 30,
	'settings.media.tts.timeout_secs': 60,
	'settings.media.image_gen.timeout_secs': 120,
};

/** Build a minimal valid wire value for focused command-contract tests. */
export function createWireValueFixture(shape: GeneratedWireShape, path = 'settings'): unknown {
	switch (shape.kind) {
		case 'string':
			return '';
		case 'number':
			if (SETTINGS_NUMERIC_FIXTURE_VALUES[path] !== undefined) {
				return SETTINGS_NUMERIC_FIXTURE_VALUES[path];
			}
			return path.endsWith('.compaction_ratio') ? 0.5 : 1;
		case 'boolean':
			return false;
		case 'unknown':
			return null;
		case 'nullable':
			return null;
		case 'array':
			return [];
		case 'record':
			return {};
		case 'enum':
			return shape.values[0] ?? '';
		case 'object':
			return Object.fromEntries(
				Object.entries(shape.fields)
					.filter(([, field]) => !field.optional)
					.map(([name, field]) => [
						name,
						createWireValueFixture(field.shape, `${path}.${name}`),
					]),
			);
	}
}

export function createSettingsFixture(): SettingsPayload {
	const settings = createWireValueFixture(SETTINGS_RESPONSE_SHAPE) as SettingsPayload;
	settings.media.stt.provider_name = 'llm';
	settings.media.ocr.provider = 'llm';
	settings.media.tts.provider_name = 'none';
	settings.media.image_gen.provider_name = 'none';
	return settings;
}
