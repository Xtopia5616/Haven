/** Shared field accessors for untrusted wire records. */

import { isNonEmptyString, isString } from './valueGuards.ts';

export type WireRecord = Record<string, unknown>;

/** Canonical `{prefix}-{uuid32}` identity check for known Rust entity fields. */
export function isCanonicalEntityId(value: unknown, prefix: string): value is string {
	const canonicalPrefix = /^[a-z0-9]+$/.exec(prefix)?.[0];
	if (
		typeof value !== 'string' ||
		canonicalPrefix !== prefix ||
		value.length !== prefix.length + 1 + 32
	) {
		return false;
	}
	const uuid = value.slice(prefix.length + 1);
	return value.startsWith(`${prefix}-`) && /^[0-9a-f]{32}$/.test(uuid);
}

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

/** Read an entity identity only when it has the expected canonical prefix. */
export function canonicalIdField(record: WireRecord, field: string, prefix: string): string | null {
	const value = record[field];
	return isCanonicalEntityId(value, prefix) ? value : null;
}

/** Accept an omitted identity or a canonical identity with the expected prefix. */
export function optionalCanonicalIdFieldIsValid(
	record: WireRecord,
	field: string,
	prefix: string,
): boolean {
	const value = record[field];
	return value === undefined || isCanonicalEntityId(value, prefix);
}

export function optionalStringFieldIsValid(record: WireRecord, field: string): boolean {
	const value = record[field];
	return value === undefined || isString(value);
}
