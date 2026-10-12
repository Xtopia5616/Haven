import { SETTINGS_RESPONSE_SHAPE } from './generatedCommands.ts';
import type { SettingsPayload } from './settings.ts';
import type { GeneratedWireShape } from './generatedWireShape.ts';

/** Build a minimal valid wire value for focused command-contract tests. */
export function createWireValueFixture(shape: GeneratedWireShape, path = 'settings'): unknown {
	switch (shape.kind) {
		case 'string':
			return '';
		case 'number':
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
	return createWireValueFixture(SETTINGS_RESPONSE_SHAPE) as SettingsPayload;
}
