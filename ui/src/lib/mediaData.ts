/** Build a data URL from an attachment with base64 payload. */
export function mediaDataUrl(att: { media_type: string; data: string }) {
	return `data:${att.media_type};base64,${att.data}`;
}
