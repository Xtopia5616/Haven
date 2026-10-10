/** Shared primitive guards for untrusted values and closed string vocabularies. */

export function isString(value: unknown): value is string {
	return typeof value === 'string';
}

export function isBoolean(value: unknown): value is boolean {
	return typeof value === 'boolean';
}

export function isNonEmptyString(value: unknown): value is string {
	return isString(value) && value.length > 0;
}

/** Accept any JavaScript number, including non-finite values. */
export function isNumber(value: unknown): value is number {
	return typeof value === 'number';
}

export function isFiniteNumber(value: unknown): value is number {
	return isNumber(value) && Number.isFinite(value);
}

export function isStringArray(value: unknown): value is string[] {
	return Array.isArray(value) && value.every(isString);
}

export function isOneOf<const Values extends readonly string[]>(
	value: unknown,
	values: Values,
): value is Values[number] {
	return typeof value === 'string' && values.includes(value);
}
