/** App-shell IPC event contract at the frontend boundary. */

import type { TauriEvent } from './session.ts';

export const APP_EVENT_NAMES = [
	'app:bootstrap',
	'tray:status_changed',
	'mute:changed',
	'mcp:status_change',
	'skills:status_change',
	'confirm:requested',
	'hotkey:conflict',
	'hotkey:rebind',
	'llm:config_changed',
] as const;

export type AppEventName = (typeof APP_EVENT_NAMES)[number];
export type RiskLevel = 'safe' | 'low' | 'medium' | 'high' | 'critical';
export type McpStatus =
	| 'Disconnected'
	| 'Connecting'
	| 'Connected'
	| { Offline: { error: string } };

export interface AppBootstrapPayload { status: 'loading' | 'ready'; }
export interface TrayStatusPayload { status: string; tooltip: string; }
export interface MuteChangedPayload { muted: boolean; }
export interface McpStatusPayload { name: string; status: McpStatus; }
export interface SkillsStatusPayload { op: string; }
export interface ConfirmationRequestedPayload {
	stepId: string;
	invocationStepId: string | null;
	actionIndex: number;
	toolCallId: string | null;
	toolName: string;
	riskLevel: RiskLevel;
	sessionId: string;
	summary: string;
	permissionKey: string;
}
export interface HotkeyConflictPayload { binding: string; error: string; }
export interface HotkeyRebindPayload { oldBinding: string; newBinding: string; }

export interface AppEventPayloadMap {
	'app:bootstrap': AppBootstrapPayload;
	'tray:status_changed': TrayStatusPayload;
	'mute:changed': MuteChangedPayload;
	'mcp:status_change': McpStatusPayload;
	'skills:status_change': SkillsStatusPayload;
	'confirm:requested': ConfirmationRequestedPayload;
	'hotkey:conflict': HotkeyConflictPayload;
	'hotkey:rebind': HotkeyRebindPayload;
	'llm:config_changed': null;
}

interface AppWirePayloadMap {
	'app:bootstrap': { status: 'loading' | 'ready' };
	'tray:status_changed': { status: string; tooltip: string };
	'mute:changed': { muted: boolean };
	'mcp:status_change': { name: string; status: McpStatus };
	'skills:status_change': { op: string };
	'confirm:requested': {
		step_id: string;
		invocation_step_id: string | null;
		action_index: number;
		tool_call_id: string | null;
		tool_name: string;
		risk_level: RiskLevel;
		session_id: string;
		summary: string;
		permission_key: string;
	};
	'hotkey:conflict': { binding: string; error: string };
	'hotkey:rebind': { old_binding: string; new_binding: string };
	'llm:config_changed': null;
}

/** Convert one known app-shell event from the Rust/Tauri wire shape. */
export function mapAppEvent<K extends AppEventName>(
	event: TauriEvent<AppWirePayloadMap[K]> & { event: K },
): TauriEvent<AppEventPayloadMap[K]> {
	const p = event.payload;
	switch (event.event) {
		case 'app:bootstrap':
			return { ...event, payload: p } as unknown as TauriEvent<AppEventPayloadMap[K]>;
		case 'tray:status_changed':
			return { ...event, payload: p } as unknown as TauriEvent<AppEventPayloadMap[K]>;
		case 'mute:changed':
			return { ...event, payload: p } as unknown as TauriEvent<AppEventPayloadMap[K]>;
		case 'mcp:status_change':
			return { ...event, payload: p } as unknown as TauriEvent<AppEventPayloadMap[K]>;
		case 'skills:status_change':
			return { ...event, payload: p } as unknown as TauriEvent<AppEventPayloadMap[K]>;
		case 'confirm:requested': {
			const payload = p as AppWirePayloadMap['confirm:requested'];
			return { ...event, payload: {
				stepId: payload.step_id,
				invocationStepId: payload.invocation_step_id,
				actionIndex: payload.action_index,
				toolCallId: payload.tool_call_id,
				toolName: payload.tool_name,
				riskLevel: payload.risk_level,
				sessionId: payload.session_id,
				summary: payload.summary,
				permissionKey: payload.permission_key,
			} } as unknown as TauriEvent<AppEventPayloadMap[K]>;
		}
		case 'hotkey:conflict': {
			const payload = p as AppWirePayloadMap['hotkey:conflict'];
			return { ...event, payload: {
				binding: payload.binding,
				error: payload.error,
			} } as unknown as TauriEvent<AppEventPayloadMap[K]>;
		}
		case 'hotkey:rebind': {
			const payload = p as AppWirePayloadMap['hotkey:rebind'];
			return { ...event, payload: {
				oldBinding: payload.old_binding,
				newBinding: payload.new_binding,
			} } as unknown as TauriEvent<AppEventPayloadMap[K]>;
		}
		case 'llm:config_changed':
			return { ...event, payload: null } as unknown as TauriEvent<AppEventPayloadMap[K]>;
	}
}
