import { isRecord } from './contracts/objectGuards.ts';
import {
	isBoolean,
	isFiniteNumber,
	isNonEmptyString,
	isString,
	isStringArray,
} from './contracts/valueGuards.ts';
import { isMediaRepresentationKind } from './contracts/media.ts';
import { isSessionStatus } from './contracts/session.ts';
import { isScheduleMode } from './contracts/schedule.ts';
import { isCanonicalEntityId } from './contracts/wireGuards.ts';
import { isToolRunStatus } from './contracts/toolRun.ts';
import {
	isToolAgentPresenceStatus,
	isToolAcPowerState,
	isToolBatteryState,
	isToolFileOperation,
	isToolFileSearchMode,
	isToolFileSymbolKind,
	isToolInputButton,
	isToolInputOperation,
	isToolMemoryOperation,
	isToolMediaFileKind,
	isToolMediaModality,
	isToolMemoryRecallMode,
	isToolRunResultStatus,
	isToolSystemScope,
	isToolMediaOperation,
	isToolRunsResultOperation,
	isToolScheduleResultOperation,
	isToolWindowControlType,
	isToolWindowFormat,
	isToolWindowOperation,
	isToolWindowWaitCondition,
} from './toolResultPresentation.ts';

type JsonRecord = Record<string, unknown>;
type FieldGuard = (value: unknown) => boolean;

const isStringOrNumber = (value: unknown): boolean => isString(value) || isFiniteNumber(value);
const isCanonicalAssetId = (value: unknown): boolean => isCanonicalEntityId(value, 'asset');
const isCanonicalToolRunId = (value: unknown): boolean => isCanonicalEntityId(value, 'toolrun');
const isMediaRepresentationArray = (value: unknown): boolean =>
	Array.isArray(value) && value.every(isMediaRepresentationKind);

function hasValidOptionalCanonicalId(data: JsonRecord, key: string, prefix: string): boolean {
	return !(key in data) || isCanonicalEntityId(data[key], prefix);
}

function hasValidOptionalFields(
	data: JsonRecord,
	fields: Readonly<Record<string, FieldGuard>>,
): boolean {
	return Object.entries(fields).every(([key, guard]) => {
		if (!(key in data) || data[key] == null) return true;
		return guard(data[key]);
	});
}

function hasValidRequiredFields(
	data: JsonRecord,
	fields: Readonly<Record<string, FieldGuard>>,
): boolean {
	return Object.entries(fields).every(([key, guard]) => guard(data[key]));
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

function hasValidUniqueRecordArray(
	data: JsonRecord,
	key: string,
	identityField: string,
	identityGuard: FieldGuard,
	validate: (value: JsonRecord) => boolean,
): boolean {
	if (!(key in data)) return true;
	if (!Array.isArray(data[key])) return false;
	const identities = new Set<string>();
	return data[key].every((item: unknown) => {
		if (!isRecord(item) || !validate(item)) return false;
		const identity = item[identityField];
		if (typeof identity !== 'string' || !identityGuard(identity) || identities.has(identity)) {
			return false;
		}
		identities.add(identity);
		return true;
	});
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

function hasValidRequiredObjectFields(
	data: JsonRecord,
	fields: Readonly<Record<string, Readonly<Record<string, FieldGuard>>>>,
): boolean {
	return Object.entries(fields).every(([key, requiredFields]) => {
		if (!(key in data) || data[key] == null) return true;
		const nested = data[key];
		if (!isRecord(nested)) return false;
		return Object.entries(requiredFields).every(([field, guard]) => guard(nested[field]));
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
			scope: isToolSystemScope,
			...stringFields('path', 'name', 'value', 'note', 'reason'),
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
			},
		}) &&
		hasValidRequiredObjectFields(data, {
			cpu: {
				cores: isFiniteNumber,
				logical_cpus: isFiniteNumber,
			},
			memory: {
				used_bytes: isFiniteNumber,
				total_bytes: isFiniteNumber,
			},
			network_summary: {
				interface_count: isFiniteNumber,
				up_or_unknown: isFiniteNumber,
				down: isFiniteNumber,
			},
		}) &&
		hasValidRecordArray(data, 'networks', (row) =>
			hasValidRequiredFields(row, {
				name: isString,
				state: isString,
				ips: isStringArray,
			}),
		) &&
		hasValidRecordArray(data, 'disks', (row) =>
			hasValidRequiredFields(row, {
				mount: isString,
				total_bytes: isFiniteNumber,
				available_bytes: isFiniteNumber,
			}),
		) &&
		hasValidRecordArray(data, 'displays', (row) => {
			if (row.available === false && !('name' in row)) {
				return hasValidOptionalFields(row, { note: isString });
			}
			return hasValidRequiredFields(row, {
				name: isString,
				width: isFiniteNumber,
				height: isFiniteNumber,
				primary: isBoolean,
			});
		}) &&
		hasValidUniqueRecordArray(
			data,
			'windows',
			'window_id',
			isNonEmptyString,
			(row) =>
				hasValidRequiredFields(row, { window_id: isNonEmptyString }) &&
				hasValidOptionalFields(row, {
					hwnd: isStringOrNumber,
					title: isString,
					pid: isFiniteNumber,
				}),
		) &&
		(['values', 'subkeys'] as const).every((key) =>
			hasValidOptionalArrayField(data, key, isStringArray),
		) &&
		hasValidUniqueRecordArray(
			data,
			'variables',
			'name',
			isNonEmptyString,
			(row) =>
				hasValidRequiredFields(row, { name: isNonEmptyString }) &&
				hasValidOptionalFields(row, { value: isString }),
		)
	);
}

function validWindowData(data: JsonRecord): boolean {
	return (
		hasValidOptionalFields(data, {
			operation: isToolWindowOperation,
			format: isToolWindowFormat,
			condition: isToolWindowWaitCondition,
			...stringFields('note', 'title', 'focused', 'closed', 'reason', 'text'),
			asset_id: isCanonicalAssetId,
			...numberFields('count', 'pid', 'width', 'height'),
			...booleanFields(
				'available',
				'success',
				'matched',
				'timed_out',
				'verified_foreground',
				'verified_closed',
			),
		}) &&
		hasValidObjectFields(data, {
			media: { asset_id: isCanonicalAssetId },
			foreground_window: {
				window_id: isString,
				hwnd: isStringOrNumber,
				title: isString,
				pid: isFiniteNumber,
			},
		}) &&
		hasValidUniqueRecordArray(
			data,
			'windows',
			'window_id',
			isNonEmptyString,
			(row) =>
				hasValidRequiredFields(row, { window_id: isNonEmptyString }) &&
				hasValidOptionalFields(row, {
					hwnd: isStringOrNumber,
					title: isString,
					pid: isFiniteNumber,
				}),
		) &&
		hasValidUniqueRecordArray(
			data,
			'elements',
			'element_token',
			(value) => typeof value === 'string' && value.trim().length > 0,
			(row) =>
				hasValidRequiredFields(row, { element_token: isString }) &&
				hasValidOptionalFields(row, {
					name: isString,
					control_type: isToolWindowControlType,
				}),
		)
	);
}

function validMediaData(data: JsonRecord): boolean {
	return (
		hasValidOptionalFields(data, {
			operation: isToolMediaOperation,
			...stringFields('reason', 'error', 'transcript'),
			asset_id: isCanonicalAssetId,
			...numberFields('duration_ms', 'characters', 'volume'),
			...booleanFields('available', 'played', 'muted', 'capture_error'),
			representation: isMediaRepresentationKind,
			modality: isToolMediaModality,
			file_kind: isToolMediaFileKind,
		}) &&
		hasValidObjectFields(data, {
			media: {
				asset_id: isCanonicalAssetId,
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
		isCanonicalEntityId(row.session_id, 'ses') &&
		hasValidOptionalFields(row, stringFields('title')) &&
		(!('status' in row) || isSessionStatus(row.status))
	);
}

function validAdminData(data: JsonRecord): boolean {
	return (
		hasValidOptionalFields(data, {
			...stringFields('level', 'name', 'path', 'error'),
			...numberFields('total_lines'),
			...booleanFields('saved', 'enabled', 'created', 'removed'),
			connected: (value) => isBoolean(value) || Array.isArray(value),
		}) &&
		hasValidOptionalArrayField(data, 'lines', isStringArray) &&
		hasValidUniqueRecordArray(
			data,
			'skills',
			'name',
			isNonEmptyString,
			(row) =>
				hasValidRequiredFields(row, { name: isNonEmptyString }) &&
				hasValidOptionalFields(row, {
					name: isString,
					enabled: isBoolean,
					executable: isBoolean,
					unavailable_reason: isString,
					manifest_error: isString,
				}),
		) &&
		hasValidRecordArray(data, 'sessions', validAdminActivityRow) &&
		hasValidRecordArray(data, 'errors', validAdminActivityRow)
	);
}

function validFileData(data: JsonRecord): boolean {
	return (
		hasValidOptionalFields(data, {
			operation: isToolFileOperation,
			...stringFields(
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
				kind: isToolFileSymbolKind,
				name: isString,
			}),
		)
	);
}

function validBuiltinRendererData(renderer: string, data: JsonRecord): boolean {
	switch (renderer) {
		case 'files.search':
			if (
				!hasValidOptionalFields(data, {
					count: isFiniteNumber,
					mode: isToolFileSearchMode,
					search_complete: isBoolean,
					io_error_count: (value) =>
						isFiniteNumber(value) && Number.isInteger(value) && value >= 0,
					has_more: isBoolean,
					truncated: isBoolean,
				}) ||
				!hasValidRecordArray(data, 'results', validFileSearchResult)
			) {
				return false;
			}
			return !(
				data.search_complete === true &&
				((typeof data.io_error_count === 'number' && data.io_error_count > 0) ||
					data.has_more === true ||
					data.truncated === true)
			);
		case 'files':
			return validFileData(data);
		case 'media':
			return validMediaData(data);
		case 'window':
			return validWindowData(data);
		case 'system':
			return validSystemData(data);
		case 'input':
			return (
				hasValidOptionalFields(data, {
					operation: isToolInputOperation,
					button: isToolInputButton,
					typed: isString,
					pressed: isString,
					foreground_window_id: isString,
					foreground_title: isString,
					...numberFields(
						'chars',
						'scrolled',
						'accepted_events',
						'foreground_hwnd',
						'foreground_pid',
					),
					...booleanFields('verified_foreground'),
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
				}) &&
				hasValidUniqueRecordArray(
					data,
					'entries',
					'entry_id',
					(value) => isCanonicalEntityId(value, 'clip'),
					(row) =>
						isString(row.content) &&
						row.content.length > 0 &&
						isCanonicalEntityId(row.entry_id, 'clip'),
				) &&
				(!Array.isArray(data.entries) ||
					data.entries.length === 0 ||
					isFiniteNumber(data.total))
			);
		case 'agent':
			return (
				hasValidOptionalFields(data, {
					...stringFields('text', 'role'),
					...numberFields('timeout_secs', 'running_sessions', 'max_concurrent'),
					...booleanFields('auto', 'timed_out', 'ok', 'queued'),
				}) &&
				hasValidOptionalCanonicalId(data, 'message_id', 'msg') &&
				hasValidOptionalCanonicalId(data, 'session_id', 'ses') &&
				hasValidOptionalCanonicalId(data, 'parent_session_id', 'ses') &&
				hasValidRecordArray(
					data,
					'agents',
					(row) =>
						isCanonicalEntityId(row.session_id, 'ses') &&
						hasValidOptionalFields(row, stringFields('title', 'role')) &&
						isToolAgentPresenceStatus(row.status),
				)
			);
		case 'toolrun':
			return (
				hasValidOptionalFields(data, {
					operation: isToolRunsResultOperation,
					status: isToolRunResultStatus,
					...booleanFields('cancelled'),
					...stringFields('tool_name', 'output'),
				}) &&
				hasValidOptionalCanonicalId(data, 'tool_run_id', 'toolrun') &&
				hasValidRecordArray(
					data,
					'tool_runs',
					(row) => isCanonicalToolRunId(row.tool_run_id) && isToolRunStatus(row.status),
				)
			);
		case 'schedule':
			return (
				hasValidOptionalFields(data, {
					operation: isToolScheduleResultOperation,
					...stringFields('fires_at'),
					mode: isScheduleMode,
				}) &&
				hasValidOptionalCanonicalId(data, 'tool_run_id', 'toolrun') &&
				hasValidOptionalCanonicalId(data, 'schedule_id', 'sched') &&
				hasValidRecordArray(
					data,
					'schedules',
					(row) =>
						isCanonicalEntityId(row.schedule_id, 'sched') &&
						isString(row.title) &&
						isString(row.body) &&
						isScheduleMode(row.mode) &&
						((row.due_at === undefined || row.due_at === null || isString(row.due_at)) &&
							(row.next_due_at === undefined ||
								row.next_due_at === null ||
								isString(row.next_due_at))),
				)
			);
		case 'admin':
			return validAdminData(data);
		case 'http':
			return (
				isFiniteNumber(data.status) &&
				hasValidOptionalFields(data, {
					truncated: isBoolean,
					body: isString,
				})
			);
		case 'web_search':
			return (
				hasValidOptionalFields(data, { label: isString, answer: isString }) &&
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
					operation: isToolMemoryOperation,
					mode: isToolMemoryRecallMode,
					deleted: isFiniteNumber,
				}) &&
				(!('hits' in data) || data.kind === 'fact' || data.kind === 'episode') &&
				hasValidObjectFields(data, { stored: stringFields('predicate', 'object') }) &&
				hasValidUniqueRecordArray(
					data,
					'facts',
					'fact_id',
					(value) => isCanonicalEntityId(value, 'fact'),
					(row) =>
						hasValidOptionalFields(row, {
							...stringFields(
								'id',
								'subject',
								'predicate',
								'object',
								'source_snippet',
							),
							confidence: isFiniteNumber,
							tags: isStringArray,
						}) && isCanonicalEntityId(row.fact_id, 'fact'),
				) &&
				hasValidUniqueRecordArray(
					data,
					'hits',
					'entity_id',
					(value) => isCanonicalEntityId(value, data.kind === 'fact' ? 'fact' : 'msg'),
					(row) =>
						hasValidOptionalFields(row, {
							...stringFields('text', 'model'),
							score: isFiniteNumber,
						}) &&
						isCanonicalEntityId(row.entity_id, data.kind === 'fact' ? 'fact' : 'msg'),
				)
			);
		case 'shell':
			return (
				hasValidOptionalFields(data, {
					truncated: isBoolean,
					exit_code: isFiniteNumber,
					output: isString,
				})
			);
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
