import { describe, expect, it } from 'vitest';
import {
	createHook,
	emptyHooks,
	hookMatches,
	hookValidationErrors,
	parseHooksImport,
} from './lifecycleHooksEditor.ts';

describe('Hook configuration editor', () => {
	it('matches Rust wildcard semantics, literals, Unicode and no-tool events', () => {
		const hook = createHook();
		expect(hookMatches(hook, null)).toBe(true);
		for (const [matcher, name, matches] of [
			['*', null, true],
			['**', null, false],
			['shell', 'Shell', false],
			['mcp_*.search', 'mcp_drive.search', true],
			['mcp_*.search', 'mcp_driveXsearch', false],
			['files.*.read', 'files.workspace.read', true],
			['files.*.read', 'files.read', false],
			['a?b', 'acb', false],
			['a?b', 'a?b', true],
			['工具*', '工具😀', true],
			['[a]+', '[a]+', true],
			['shell', null, false],
			['a**b', 'axxb', true],
		] as const)
			expect(hookMatches({ ...hook, matcher }, name)).toBe(matches);
		expect(hookMatches({ ...hook, enabled: false }, 'shell')).toBe(false);
	});

	it('bounds work for a pathological wildcard pattern', () => {
		expect(
			hookMatches({ ...createHook(), matcher: '*a'.repeat(60) + 'b' }, 'a'.repeat(256)),
		).toBe(false);
	});

	it('uses UTF-8 byte limits, control checks and disabled command validation', () => {
		const hook = { ...createHook(), command: 'hook.exe' };
		expect(hookValidationErrors(hook)).toEqual([]);
		expect(hookValidationErrors({ ...hook, command: '中'.repeat(683) })).toHaveLength(1);
		expect(hookValidationErrors({ ...hook, args: ['\u0085'] })).toHaveLength(1);
		expect(hookValidationErrors({ ...hook, args: Array(33).fill('x') })).toHaveLength(1);
		expect(hookValidationErrors({ ...hook, matcher: 'shell *' })).toHaveLength(1);
		expect(hookValidationErrors({ ...hook, matcher: 'shell\u0085' })).toHaveLength(1);
		expect(hookValidationErrors({ ...hook, matcher: '\ufeff' })).toEqual([]);
		expect(hookValidationErrors({ ...hook, enabled: false, command: '' })).toHaveLength(1);
	});

	it('imports older defaults and round trips disabled configurations independently', () => {
		const imported = parseHooksImport('{"run_start":[{"command":"hook.exe"}]}');
		expect(imported.run_start[0]).toEqual({
			enabled: true,
			command: 'hook.exe',
			args: [],
			matcher: null,
		});
		imported.run_start[0].enabled = false;
		expect(parseHooksImport(JSON.stringify(imported))).toEqual(imported);
		const first = createHook('powershell');
		first.args.push('extra');
		expect(createHook('powershell').args).not.toContain('extra');
		expect(emptyHooks().pre_tool_use).toEqual([]);
	});

	it.each([
		'null',
		'[]',
		'{',
		'{"unknown":[]}',
		'{"__proto__":[]}',
		'{"run_start":[{"command":"x","enabled":"false"}]}',
		'{"run_start":[{"command":"x","args":[3]}]}',
		'{"run_start":[{"command":"x","matcher":42}]}',
		'{"run_start":[{"command":"x","extra":true}]}',
		'{"run_start":[{"command":""}]}',
		JSON.stringify({ run_start: Array(9).fill({ command: 'x' }) }),
	])('rejects invalid input without mutating a draft: %s', (text) => {
		expect(() => parseHooksImport(text)).toThrow();
	});

	it('rejects oversized imports before parsing', () => {
		expect(() => parseHooksImport(' '.repeat(8 * 1024 * 1024 + 1))).toThrow('8 MiB');
	});
});
