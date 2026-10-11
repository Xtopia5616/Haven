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

/** Accept an integer that JavaScript can represent exactly, including negatives. */
export function isSafeInteger(value: unknown): value is number {
	return Number.isSafeInteger(value);
}

/** Accept a non-negative integer that JavaScript can represent exactly. */
export function isSafeUnsignedInteger(value: unknown): value is number {
	return isSafeInteger(value) && (value as number) >= 0;
}

/** Match Rust's u32 domain at a JSON boundary. */
export function isU32(value: unknown): value is number {
	return isSafeUnsignedInteger(value) && value <= 0xffff_ffff;
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
