import {
	MEDIA_REPRESENTATION_KIND_VALUES,
	type MediaRepresentationKind,
} from './generatedCommands.ts';

export function isMediaRepresentationKind(value: unknown): value is MediaRepresentationKind {
	return (MEDIA_REPRESENTATION_KIND_VALUES as readonly unknown[]).includes(value);
}
