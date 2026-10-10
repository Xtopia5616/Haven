import {
	MEDIA_REPRESENTATION_KIND_VALUES,
	type MediaRepresentationKind,
} from './generatedCommands.ts';
import { isOneOf } from './valueGuards.ts';

export function isMediaRepresentationKind(value: unknown): value is MediaRepresentationKind {
	return isOneOf(value, MEDIA_REPRESENTATION_KIND_VALUES);
}
