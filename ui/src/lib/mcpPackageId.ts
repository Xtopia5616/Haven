/** Administrator-managed package catalog IDs accepted by the Rust config boundary. */
export function isValidMcpPackageId(value: string): boolean {
	return (
		value.length > 0 &&
		value.length <= 64 &&
		/^[a-z]/u.test(value) &&
		!/[^a-z0-9-]/u.test(value)
	);
}
