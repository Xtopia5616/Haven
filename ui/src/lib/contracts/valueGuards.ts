/** Shared primitive guards for untrusted values and closed string vocabularies. */

export function isString(value: unknown): value is string {
	return typeof value === 'string';
}

/** Accept UTF-8 text only when its encoded byte length fits the storage contract. */
export function isUtf8StringWithinBytes(value: unknown, maxBytes: number): value is string {
	return typeof value === 'string' && new TextEncoder().encode(value).byteLength <= maxBytes;
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

/** Accept a finite monetary amount in the non-negative USD domain. */
export function isNonNegativeFiniteNumber(value: unknown): value is number {
	return isFiniteNumber(value) && value >= 0;
}

/** Match the Rust CapabilityScope grammar used by persisted permission grants. */
export function isCapabilityScope(value: unknown): value is string {
	return (
		typeof value === 'string' &&
		value.length > 0 &&
		value.trim() === value &&
		value.split('.').every((segment) => /^[A-Za-z0-9_-]+$/.test(segment))
	);
}

/** Accept an integer that JavaScript can represent exactly, including negatives. */
export function isSafeInteger(value: unknown): value is number {
	return Number.isSafeInteger(value);
}

/** Accept a non-negative integer that JavaScript can represent exactly. */
export function isSafeUnsignedInteger(value: unknown): value is number {
	return isSafeInteger(value) && (value as number) >= 0;
}

/** Match Rust's i32 domain at a JSON boundary. */
export function isI32(value: unknown): value is number {
	return (
		typeof value === 'number' &&
		Number.isInteger(value) &&
		value >= -0x8000_0000 &&
		value <= 0x7fff_ffff
	);
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
