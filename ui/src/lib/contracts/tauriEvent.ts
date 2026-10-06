/** Generic event envelope emitted by Tauri's renderer listener API. */
export interface TauriEvent<T> {
	event: string;
	id: number;
	payload: T;
}
