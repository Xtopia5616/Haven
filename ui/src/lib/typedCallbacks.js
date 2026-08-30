/**
 * Provide type context for callback expressions in Svelte markup.
 *
 * Svelte's compiler currently mishandles JSDoc placed directly on template
 * callback parameters, so the type belongs on the callback boundary instead.
 * These helpers are identity functions at runtime.
 *
 * @param {(value: string) => void} callback
 * @returns {(value: string) => void}
 */
export function withStringValue(callback) {
	return callback;
}

/**
 * @param {(value: number) => void} callback
 * @returns {(value: number) => void}
 */
export function withNumberValue(callback) {
	return callback;
}

/**
 * @param {(value: boolean) => void} callback
 * @returns {(value: boolean) => void}
 */
export function withBooleanValue(callback) {
	return callback;
}

/**
 * @param {(value: any) => void} callback
 * @returns {(value: any) => void}
 */
export function withAnyValue(callback) {
	return callback;
}

/**
 * @param {(value: Event) => void} callback
 * @returns {(value: Event) => void}
 */
export function withEventValue(callback) {
	return callback;
}

/**
 * @param {Event} event
 * @returns {string}
 */
export function inputElementValue(event) {
	return /** @type {HTMLInputElement} */ (event.currentTarget).value;
}
