import { describe, expect, it } from 'vitest';
import {
	matchesGeneratedEventPayload,
	matchesGeneratedWireShape,
	type GeneratedWireShape,
} from './generatedWireShape.ts';

describe('generated Rust wire shapes', () => {
	const shape: GeneratedWireShape = {
		kind: 'object',
		fields: {
			name: { optional: false, shape: { kind: 'string' } },
			mode: { optional: false, shape: { kind: 'enum', values: ['safe', 'fast'] } },
			labels: { optional: false, shape: { kind: 'array', items: { kind: 'string' } } },
			metadata: {
				optional: false,
				shape: { kind: 'record', values: { kind: 'nullable', inner: { kind: 'number' } } },
			},
			optional_note: { optional: true, shape: { kind: 'string' } },
		},
	};

	it('checks required fields, closed enums, containers, nullability, and present optional fields', () => {
		expect(
			matchesGeneratedWireShape(
				{
					name: 'Haven',
					mode: 'safe',
					labels: ['one'],
					metadata: { count: 2, note: null },
					optional_note: 'future proof',
				},
				shape,
			),
		).toBe(true);
	});

	it('allows absent optional properties and additive response fields', () => {
		expect(
			matchesGeneratedWireShape(
				{ name: 'Haven', mode: 'fast', labels: [], metadata: {}, future_field: true },
				shape,
			),
		).toBe(true);
	});

	it('rejects incomplete and malformed nested response values', () => {
		for (const invalid of [
			{ mode: 'safe', labels: [], metadata: {} },
			{ name: 'Haven', mode: 'unknown', labels: [], metadata: {} },
			{ name: 'Haven', mode: 'safe', labels: [1], metadata: {} },
			{ name: 'Haven', mode: 'safe', labels: [], metadata: { count: '2' } },
			{ name: 'Haven', mode: 'safe', labels: [], metadata: {}, optional_note: null },
		]) {
			expect(matchesGeneratedWireShape(invalid, shape)).toBe(false);
		}
	});

	it('checks tagged unions, string literals, null, and fixed tuples', () => {
		const shape: GeneratedWireShape = {
			kind: 'union',
			anyOf: [
				{ kind: 'null' },
				{
					kind: 'object',
					fields: {
						kind: { optional: false, shape: { kind: 'literal', value: 'text' } },
						value: {
							optional: false,
							shape: {
								kind: 'tuple',
								items: [{ kind: 'number' }, { kind: 'boolean' }],
							},
						},
					},
				},
				{
					kind: 'object',
					fields: {
						kind: { optional: false, shape: { kind: 'literal', value: 'empty' } },
						nullable: {
							optional: false,
							shape: { kind: 'nullable', inner: { kind: 'string' } },
						},
					},
				},
			],
		};

		expect(matchesGeneratedWireShape({ kind: 'text', value: [4, true] }, shape)).toBe(true);
		expect(matchesGeneratedWireShape(null, shape)).toBe(true);
		expect(matchesGeneratedWireShape({ kind: 'empty', nullable: null }, shape)).toBe(true);
		expect(matchesGeneratedWireShape({ kind: 'text', value: [4] }, shape)).toBe(false);
		expect(matchesGeneratedWireShape({ kind: 'other', value: [4, true] }, shape)).toBe(false);
	});

	it('uses Rust-generated payload shapes for every registered event channel', () => {
		expect(
			matchesGeneratedEventPayload('recording:started', {
				is_recording: true,
				recording_id: 'rec-83ead25b7e0ce1ca7d61f0ef95e4f29c',
			}),
		).toBe(true);
		expect(matchesGeneratedEventPayload('recording:started', { recording_id: 'missing-state' })).toBe(
			false,
		);
		expect(
			matchesGeneratedEventPayload('tool_run:created', {
				tool_run_id: 'toolrun-58ec300e6432076c8ec176f56120ec01',
				kind: 'background',
				status: 'future_status',
			}),
		).toBe(false);
		expect(matchesGeneratedEventPayload('unknown:event', {})).toBe(false);
	});
});
