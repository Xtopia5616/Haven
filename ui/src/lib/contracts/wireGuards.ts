/** Shared field accessors for untrusted wire records. */

import { isNonEmptyString, isString } from './valueGuards.ts';

export type WireRecord = Record<string, unknown>;

export function hasOwnWireField(record: WireRecord, field: string): boolean {
	return Object.prototype.hasOwnProperty.call(record, field);
}

/** Read a string field without imposing non-empty domain semantics. */
export function readStringField(record: WireRecord, field: string): string | null {
	const value = record[field];
	return isString(value) ? value : null;
}

/** Read a non-empty string field, used for required entity identities. */
export function nonEmptyStringField(record: WireRecord, field: string): string | null {
	const value = readStringField(record, field);
	return isNonEmptyString(value) ? value : null;
}

export function optionalStringFieldIsValid(record: WireRecord, field: string): boolean {
	const value = record[field];
	return value === undefined || isString(value);
}
