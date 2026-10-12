/** Validate a scalar boolean returned across the Tauri command boundary. */
export function parseBooleanResponse(value: unknown, command: string): boolean {
	if (typeof value !== 'boolean') {
		throw new Error(`Invalid ${command} boolean response`);
	}
	return value;
}
