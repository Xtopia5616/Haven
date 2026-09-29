/** Type callback expressions in Svelte markup at the callback boundary. */
export function withStringValue(callback: (value: string) => void): (value: string) => void {
	return callback;
}

export function withNumberValue(callback: (value: number) => void): (value: number) => void {
	return callback;
}

export function withBooleanValue(callback: (value: boolean) => void): (value: boolean) => void {
	return callback;
}

export function withAnyValue(callback: (value: any) => void): (value: any) => void {
	return callback;
}

export function withEventValue(callback: (value: Event) => void): (value: Event) => void {
	return callback;
}

export function inputElementValue(event: Event): string {
	return (event.currentTarget as HTMLInputElement).value;
}
