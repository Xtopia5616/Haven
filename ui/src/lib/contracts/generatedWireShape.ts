import { isRecord } from './objectGuards.ts';

/** Runtime description emitted from Rust serde response types. */
export type GeneratedWireShape =
	| { readonly kind: 'string' | 'boolean' | 'unknown' }
	| { readonly kind: 'number' }
	| { readonly kind: 'null' }
	| { readonly kind: 'literal'; readonly value: string }
	| { readonly kind: 'nullable'; readonly inner: GeneratedWireShape }
	| { readonly kind: 'array'; readonly items: GeneratedWireShape }
	| { readonly kind: 'tuple'; readonly items: readonly GeneratedWireShape[] }
	| { readonly kind: 'record'; readonly values: GeneratedWireShape }
	| { readonly kind: 'enum'; readonly values: readonly string[] }
	| { readonly kind: 'union'; readonly anyOf: readonly GeneratedWireShape[] }
	| {
			readonly kind: 'object';
			readonly fields: Readonly<
				Record<string, { readonly optional: boolean; readonly shape: GeneratedWireShape }>
			>;
	  };

/**
 * Check an untrusted IPC value against generated Rust response metadata.
 * Object schemas intentionally allow extra fields so additive Rust response
 * fields remain forward-compatible; required and present optional fields are
 * still checked recursively.
 */
export function matchesGeneratedWireShape(value: unknown, shape: GeneratedWireShape): boolean {
	return findGeneratedWireShapeMismatch(value, shape) === null;
}

/** Return the first path that does not match generated wire metadata. */
export function findGeneratedWireShapeMismatch(
	value: unknown,
	shape: GeneratedWireShape,
	path = '$',
): string | null {
	switch (shape.kind) {
		case 'string':
			return typeof value === 'string' ? null : path;
		case 'number':
			return typeof value === 'number' && Number.isFinite(value) ? null : path;
		case 'boolean':
			return typeof value === 'boolean' ? null : path;
		case 'unknown':
			return null;
		case 'null':
			return value === null ? null : path;
		case 'literal':
			return value === shape.value ? null : path;
		case 'nullable':
			return value === null ? null : findGeneratedWireShapeMismatch(value, shape.inner, path);
		case 'array': {
			if (!Array.isArray(value)) return path;
			for (const [index, item] of value.entries()) {
				const mismatch = findGeneratedWireShapeMismatch(
					item,
					shape.items,
					`${path}[${index}]`,
				);
				if (mismatch) return mismatch;
			}
			return null;
		}
		case 'tuple': {
			if (!Array.isArray(value) || value.length !== shape.items.length) return path;
			for (const [index, itemShape] of shape.items.entries()) {
				const mismatch = findGeneratedWireShapeMismatch(
					value[index],
					itemShape,
					`${path}[${index}]`,
				);
				if (mismatch) return mismatch;
			}
			return null;
		}
		case 'record': {
			if (!isRecord(value)) return path;
			for (const [key, item] of Object.entries(value)) {
				const mismatch = findGeneratedWireShapeMismatch(
					item,
					shape.values,
					`${path}.${key}`,
				);
				if (mismatch) return mismatch;
			}
			return null;
		}
		case 'enum':
			return typeof value === 'string' && shape.values.includes(value) ? null : path;
		case 'union':
			return shape.anyOf.some((option) => matchesGeneratedWireShape(value, option))
				? null
				: path;
		case 'object':
			if (!isRecord(value)) return path;
			for (const [field, definition] of Object.entries(shape.fields)) {
				const present = Object.prototype.hasOwnProperty.call(value, field);
				if (!present) {
					if (!definition.optional) return `${path}.${field}`;
					continue;
				}
				const mismatch = findGeneratedWireShapeMismatch(
					value[field],
					definition.shape,
					`${path}.${field}`,
				);
				if (mismatch) return mismatch;
			}
			return null;
	}
}
