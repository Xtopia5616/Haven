import { isRecord } from './contracts/objectGuards.ts';
import { isMediaRepresentationKind } from './contracts/media.ts';
import { isSessionStatus } from './contracts/session.ts';
import { isToolRunStatus } from './contracts/toolRun.ts';
import {
	isToolAgentPresenceStatus,
	isToolAcPowerState,
	isToolBatteryState,
	isToolExecutionMode,
	isToolFileSearchMode,
	isToolMediaFileKind,
	isToolMediaModality,
	isToolMemoryRecallMode,
	isToolScheduleMode,
} from './toolResultPresentation.ts';

type JsonRecord = Record<string, unknown>;
type FieldGuard = (value: unknown) => boolean;

const isString = (value: unknown): value is string => typeof value === 'string';
const isBoolean = (value: unknown): value is boolean => typeof value === 'boolean';
const isFiniteNumber = (value: unknown): value is number =>
	typeof value === 'number' && Number.isFinite(value);
const isStringOrNumber = (value: unknown): boolean => isString(value) || isFiniteNumber(value);
const isStringArray = (value: unknown): boolean => Array.isArray(value) && value.every(isString);
const isMediaRepresentationArray = (value: unknown): boolean =>
	Array.isArray(value) && value.every(isMediaRepresentationKind);

function hasValidOptionalFields(
	data: JsonRecord,
	fields: Readonly<Record<string, FieldGuard>>,
): boolean {
	return Object.entries(fields).every(([key, guard]) => {
		if (!(key in data) || data[key] == null) return true;
		return guard(data[key]);
	});
}

function hasValidRecordArray(
	data: JsonRecord,
	key: string,
	validate: (value: JsonRecord) => boolean = () => true,
): boolean {
	if (!(key in data)) return true;
	return (
		Array.isArray(data[key]) &&
		data[key].every((item: unknown) => isRecord(item) && validate(item))
	);
}

function hasValidOptionalArrayField(data: JsonRecord, key: string, validate: FieldGuard): boolean {
	return !(key in data) || validate(data[key]);
}

function hasValidObjectFields(
	data: JsonRecord,
	fields: Readonly<Record<string, Readonly<Record<string, FieldGuard>>>>,
): boolean {
	return Object.entries(fields).every(([key, nestedFields]) => {
		if (!(key in data) || data[key] == null) return true;
		return isRecord(data[key]) && hasValidOptionalFields(data[key], nestedFields);
	});
}

const stringFields = (...keys: string[]): Record<string, FieldGuard> =>
	Object.fromEntries(keys.map((key) => [key, isString]));
const numberFields = (...keys: string[]): Record<string, FieldGuard> =>
	Object.fromEntries(keys.map((key) => [key, isFiniteNumber]));
const booleanFields = (...keys: string[]): Record<string, FieldGuard> =>
	Object.fromEntries(keys.map((key) => [key, isBoolean]));

function validFileSearchResult(row: JsonRecord): boolean {
	return (
		isString(row.path) &&
		hasValidOptionalFields(row, {
			line: isFiniteNumber,
			snippet: isString,
		})
	);
}

function validSystemData(data: JsonRecord): boolean {
	return (
		hasValidOptionalFields(data, {
			...stringFields('scope', 'operation', 'path', 'name', 'value', 'note', 'reason'),
			...numberFields('count', 'battery_percent'),
			ac_power: isToolAcPowerState,
			battery_status: isToolBatteryState,
			...booleanFields(
				'deleted',
				'available',
				'battery_present',
				'battery_saver',
				'locked',
				'sleep',
				'hibernate',
				'set',
				'removed',
			),
		}) &&
		hasValidObjectFields(data, {
			os: { name: isString, hostname: isString, arch: isString, uptime_secs: isFiniteNumber },
			user: {
				username: isString,
				computer_name: isString,
				home: isString,
				cwd: isString,
			},
			locale: {
				locale_name: isString,
				ui_language: isString,
				local_time: isString,
				timezone_offset_hours: isFiniteNumber,
			},
			cpu: {
				usage_pct: isFiniteNumber,
				cores: isFiniteNumber,
				logical_cpus: isFiniteNumber,
			},
			memory: {
				used_bytes: isFiniteNumber,
				total_bytes: isFiniteNumber,
				available_bytes: isFiniteNumber,
			},
			network_summary: {
				interface_count: isFiniteNumber,
				up_or_unknown: isFiniteNumber,
				down: isFiniteNumber,
			},
		}) &&
		hasValidRecordArray(
			data,
			'networks',
			(row) =>
				hasValidOptionalFields(row, { name: isString, state: isString }) &&
				hasValidOptionalArrayField(row, 'ips', isStringArray),
		) &&
		hasValidRecordArray(data, 'disks', (row) =>
			hasValidOptionalFields(row, {
				mount: isString,
				total_bytes: isFiniteNumber,
				available_bytes: isFiniteNumber,
			}),
		) &&
		hasValidRecordArray(data, 'displays', (row) =>
			hasValidOptionalFields(row, {
				name: isString,
				left: isFiniteNumber,
				width: isFiniteNumber,
				height: isFiniteNumber,
				primary: isBoolean,
			}),
		) &&
		(['values', 'subkeys'] as const).every((key) =>
			hasValidOptionalArrayField(data, key, isStringArray),
		) &&
		hasValidRecordArray(data, 'variables', (row) =>
			hasValidOptionalFields(row, { name: isString, value: isString }),
		)
	);
}

function validProcessData(data: JsonRecord): boolean {
	return (
		hasValidOptionalFields(data, {
			...stringFields('operation'),
			killed: isStringOrNumber,
		}) &&
		hasValidRecordArray(data, 'processes', (row) =>
			hasValidOptionalFields(row, {
				name: isString,
				pid: isStringOrNumber,
				cpu: isFiniteNumber,
				memory: isFiniteNumber,
				status: isString,
			}),
		)
	);
}

function validWindowData(data: JsonRecord): boolean {
	return (
		hasValidOptionalFields(data, {
			...stringFields(
				'operation',
				'note',
				'title',
				'focused',
				'closed',
				'format',
				'reason',
				'condition',
				'text',
				'asset_id',
			),
			...numberFields('count', 'pid', 'width', 'height'),
			...booleanFields('available', 'success', 'matched', 'timed_out'),
		}) &&
		hasValidObjectFields(data, {
			media: { asset_id: isString },
		}) &&
		hasValidRecordArray(data, 'windows', (row) =>
			hasValidOptionalFields(row, {
				hwnd: isStringOrNumber,
				title: isString,
				pid: isFiniteNumber,
			}),
		) &&
		hasValidRecordArray(data, 'elements', (row) =>
			hasValidOptionalFields(row, { name: isString, control_type: isString }),
		)
	);
}

function validMediaData(data: JsonRecord): boolean {
	return (
		hasValidOptionalFields(data, {
			...stringFields('operation', 'asset_id', 'reason', 'error', 'transcript'),
			...numberFields('duration_ms', 'characters', 'volume'),
			...booleanFields('available', 'played', 'muted', 'capture_error'),
			representation: isMediaRepresentationKind,
			modality: isToolMediaModality,
			file_kind: isToolMediaFileKind,
		}) &&
		hasValidObjectFields(data, {
			media: {
				asset_id: isString,
				filename: isString,
				representation: isMediaRepresentationKind,
				recommended_next: isString,
			},
		}) &&
		hasValidOptionalArrayField(
			isRecord(data.media) ? data.media : {},
			'available_representations',
			isMediaRepresentationArray,
		)
	);
}

function validAdminActivityRow(row: JsonRecord): boolean {
	return (
		hasValidOptionalFields(row, stringFields('id', 'title')) &&
		(!('status' in row) || isSessionStatus(row.status))
	);
}

function validAdminData(data: JsonRecord): boolean {
	return (
		hasValidOptionalFields(data, {
			...stringFields('level', 'name'),
			...booleanFields('saved', 'enabled', 'created', 'removed'),
			connected: (value) => isBoolean(value) || Array.isArray(value),
		}) &&
		hasValidRecordArray(data, 'servers', (row) =>
			hasValidOptionalFields(row, {
				name: isString,
				connected: isBoolean,
				tools: isFiniteNumber,
			}),
		) &&
		hasValidRecordArray(data, 'skills', (row) =>
			hasValidOptionalFields(row, { name: isString, enabled: isBoolean }),
		) &&
		hasValidRecordArray(data, 'sessions', validAdminActivityRow) &&
		hasValidRecordArray(data, 'errors', validAdminActivityRow)
	);
}

function validFileData(data: JsonRecord): boolean {
	return (
		hasValidOptionalFields(data, {
			...stringFields(
				'operation',
				'path',
				'from',
				'to',
				'description',
				'reason',
				'file_type',
				'mime',
				'summary',
				'content',
				'warning',
				'error',
			),
			...numberFields('line', 'size', 'count'),
			...booleanFields(
				'written',
				'edited',
				'copied',
				'moved',
				'deleted',
				'created',
				'image',
				'understand_error',
				'understand_unavailable',
				'binary',
				'too_large',
				'summary_error',
				'summary_unavailable',
			),
		}) &&
		hasValidOptionalArrayField(data, 'entries', isStringArray) &&
		hasValidRecordArray(data, 'symbols', (row) =>
			hasValidOptionalFields(row, {
				line: isFiniteNumber,
				kind: isString,
				name: isString,
			}),
		)
	);
}

function validBuiltinRendererData(renderer: string, data: JsonRecord): boolean {
	switch (renderer) {
		case 'files.search':
		case 'file_search':
			return (
				hasValidOptionalFields(data, {
					count: isFiniteNumber,
					mode: isToolFileSearchMode,
				}) && hasValidRecordArray(data, 'results', validFileSearchResult)
			);
		case 'files':
			return validFileData(data);
		case 'media':
			return validMediaData(data);
		case 'process':
			return validProcessData(data);
		case 'window':
			return validWindowData(data);
		case 'system':
			return validSystemData(data);
		case 'input':
			return (
				hasValidOptionalFields(data, {
					...stringFields('operation', 'typed', 'pressed', 'button'),
					...numberFields('chars', 'scrolled'),
				}) &&
				(['clicked', 'moved_to'] as const).every((key) =>
					hasValidOptionalArrayField(
						data,
						key,
						(value) =>
							Array.isArray(value) &&
							value.length === 2 &&
							value.every(isFiniteNumber),
					),
				)
			);
		case 'clipboard':
			return (
				hasValidOptionalFields(data, {
					...stringFields('content'),
					...numberFields('total'),
					...booleanFields('written'),
				}) && hasValidRecordArray(data, 'entries', (row) => isString(row.content))
			);
		case 'agent':
			return (
				hasValidOptionalFields(data, {
					...stringFields('text', 'message_id', 'session_id', 'parent', 'role'),
					...numberFields('timeout_secs', 'running_sessions', 'max_concurrent'),
					...booleanFields('auto', 'timed_out', 'ok', 'queued'),
				}) &&
				hasValidRecordArray(
					data,
					'agents',
					(row) =>
						isString(row.name) &&
						hasValidOptionalFields(row, stringFields('title', 'role')) &&
						isToolAgentPresenceStatus(row.status),
				)
			);
		case 'tool_runs':
			return (
				hasValidOptionalFields(data, {
					...stringFields('operation', 'tool_run_id', 'status'),
					...booleanFields('cancelled'),
					exit_code: isFiniteNumber,
				}) &&
				hasValidRecordArray(
					data,
					'tool_runs',
					(row) => isString(row.tool_run_id) && isToolRunStatus(row.status),
				)
			);
		case 'schedule':
			return (
				hasValidOptionalFields(data, {
					...stringFields('operation', 'tool_run_id', 'fires_at'),
					mode: isToolScheduleMode,
				}) &&
				hasValidRecordArray(
					data,
					'scheduled_tool_runs',
					(row) =>
						isString(row.tool_run_id) &&
						isString(row.title) &&
						isString(row.body) &&
						isToolScheduleMode(row.mode) &&
						isString(row.due_at),
				)
			);
		case 'admin':
			return validAdminData(data);
		case 'http':
			return hasValidOptionalFields(data, {
				status: isFiniteNumber,
				truncated: isBoolean,
				body: isString,
			});
		case 'web_search':
			return (
				hasValidOptionalFields(data, { label: isString }) &&
				hasValidOptionalArrayField(data, 'queries', isStringArray) &&
				hasValidRecordArray(
					data,
					'results',
					(row) =>
						hasValidOptionalFields(row, stringFields('title', 'url', 'snippet')) &&
						isString(row.title) &&
						isString(row.url),
				)
			);
		case 'memory':
			return (
				hasValidOptionalFields(data, {
					...stringFields('operation'),
					mode: isToolMemoryRecallMode,
					deleted: isFiniteNumber,
				}) &&
				hasValidObjectFields(data, { stored: stringFields('predicate', 'object') }) &&
				hasValidRecordArray(data, 'facts', (row) =>
					hasValidOptionalFields(row, {
						...stringFields('id', 'subject', 'predicate', 'object', 'source_snippet'),
						confidence: isFiniteNumber,
						tags: isStringArray,
					}),
				) &&
				hasValidRecordArray(data, 'hits', (row) =>
					hasValidOptionalFields(row, {
						...stringFields('entity_id', 'text', 'model'),
						score: isFiniteNumber,
					}),
				)
			);
		case 'shell':
			return hasValidOptionalFields(data, {
				truncated: isBoolean,
				execution_mode: isToolExecutionMode,
				status: isToolRunStatus,
				tool_run_id: isString,
				exit_code: isFiniteNumber,
			});
		default:
			return true;
	}
}

/**
 * Validate only fields consumed by a builtin result renderer. Tool output
 * remains open JSON; unknown fields and unknown extension renderers are kept.
 */
export function isValidBuiltinToolResultData(
	renderer: string,
	value: unknown,
): value is JsonRecord {
	return isRecord(value) && validBuiltinRendererData(renderer, value);
}
