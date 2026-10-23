# Lifecycle hooks

Haven can run user-configured commands at seven Agent lifecycle points: `pre_tool_use`, `post_tool_use`, `session_start`, `run_start`, `run_end`, `pre_compact`, and `post_compact`. User hooks are managed in Settings → Lifecycle Hooks and saved in `config.toml`. The service reads the current ConfigService snapshot for every event, so edits take effect without restarting Haven.

## User hooks

Commands start directly with literal arguments. Haven does not invoke a shell, expand variables, or interpolate event values into the command line. Event data is sent as one JSON object on stdin.

```toml
[[hooks.pre_tool_use]]
command = "pwsh.exe"
enabled = true # default; set false to pause without removing the command
args = ["-NoProfile", "-File", "C:/Users/me/haven-hooks/check-tool.ps1"]
matcher = "shell"

[[hooks.run_start]]
command = "C:/Tools/haven-session-note.exe"
```

Settings lets you add/remove Hook commands, edit the executable and literal arguments, configure tool matchers, and review the active security policy. Arguments are entered one per line in the UI. `matcher` is optional; omit it to match every tool. `*` matches any sequence of characters, so patterns such as `mcp_*.search` are supported. Matching is case-sensitive.

You can pause individual commands, duplicate them, reorder them, filter by event, and start from PowerShell, Python, or Node.js launch templates. Templates fill executable/argument fields; create the script yourself or change the path to an existing script. Paths relative to the command's working directory refer to the active execution workspace, including for user-level hooks. Do not add shell quotes around individual arguments.

The local matcher preview never starts a process. Events other than tool events have no tool name: only an omitted matcher or the literal `*` matches them. The editor warns about other patterns on those events. Paused commands retain their position, arguments and matcher, still count toward the eight-command limit, and must still pass configuration validation. Pausing takes effect after saving and applies to subsequent events; it does not cancel an already running command.

The import/export section uses a JSON object keyed by event name (the same object as `hooks`, without an outer `hooks` wrapper). Import replaces only the unsaved user-level draft; review it and save explicitly to activate it. Omitted events become empty lists. Omitted `enabled`, `args` and `matcher` use `true`, `[]` and `null`. Unknown events/fields, invalid types, invalid commands, more than eight commands per event, and imports above 8 MiB are rejected without changing the draft. Project workspace trust is never imported. Export text contains executable paths and arguments; inspect it before sharing.

```json
{
  "run_end": [
    { "enabled": false, "command": "node.exe", "args": ["scripts/hook.mjs"], "matcher": null }
  ]
}
```

Field validation covers UTF-8 byte limits, control characters, argument count and matcher whitespace. Errors remain blocking even when their event is hidden by the filter. A failed save preserves the editor and draft for correction or retry; “Discard changes” restores the last successfully loaded/saved configuration.

## Script starters

Each script reads the JSON object to EOF. These minimal starters allow tool calls; replace the decision with your own checks. Notification events ignore stdout. Keep scripts short: all commands for one event share the five-second budget. Debug messages belong on stderr because `pre_tool_use` parses stdout as its decision.

PowerShell (`scripts/hook.ps1`, launched with `pwsh.exe -NoProfile -NonInteractive -File scripts/hook.ps1`):

```powershell
$ErrorActionPreference = 'Stop'
$eventData = [Console]::In.ReadToEnd() | ConvertFrom-Json
if ($eventData.event -eq 'pre_tool_use') {
    @{ decision = 'allow' } | ConvertTo-Json -Compress
}
```

Python (`scripts/hook.py`, launched with `python.exe scripts/hook.py`):

```python
import json
import sys

event = json.load(sys.stdin)
if event['event'] == 'pre_tool_use':
    print(json.dumps({'decision': 'allow'}))
```

Node.js (`scripts/hook.mjs`, launched with `node.exe scripts/hook.mjs`):

```javascript
let input = '';
for await (const chunk of process.stdin) input += chunk;
const event = JSON.parse(input);
if (event.event === 'pre_tool_use') {
  process.stdout.write(JSON.stringify({ decision: 'allow' }));
}
```

Use `post_tool_use` for local audit checks based on `success`, `run_end` for completion integrations based on `run_outcome`, and `post_compact` for compaction metrics. Notification hooks cannot change Haven state through their response. For example, inspect `event.run_outcome === 'completed'` in a `run_end` script before updating a local completion indicator. Never insert raw tool input into executable code or shell command strings.

User-level command lists run before project-level lists for each event. Within each list, commands run in their configured order. `pre_tool_use` stops at the first deny or failure. Notification events attempt each configured command in order, subject to the event-wide five-second limit.

## Project hooks and trust

Select a local directory from Settings → Lifecycle Hooks → Project Hook Trust. Haven stores its canonical path in `trusted_hook_workspaces`. Only the exact active execution workspace root can contribute its `.haven/hooks.toml`; a parent/child directory match does not grant trust. Trust can be revoked in Settings at any time.

```toml
[hooks]

[[hooks.pre_tool_use]]
command = "pwsh.exe"
args = ["-NoProfile", "-File", "scripts/check-tool.ps1"]
matcher = "shell"

[[hooks.post_compact]]
command = "C:/Tools/project-compact-note.exe"
```

Haven reads the project file through its pinned workspace directory handle and rejects reparse points, non-regular files, invalid TOML, and files larger than 64 KiB. An untrusted workspace's `.haven/hooks.toml` is ignored without reading it. Trust authorizes the project Hook configuration only; every resulting command still passes Haven's active security policy.

## Event behavior

- `pre_tool_use` runs after Haven's normal tool authorization and confirmation, immediately before tool execution. A command can allow or deny the call by writing `{"decision":"allow"}` or `{"decision":"deny","reason":"..."}` to stdout. Empty stdout means allow; a nonzero exit, invalid reply, timeout, cancellation, or security-policy rejection denies the call. Allow never bypasses Haven's authorization.
- `post_tool_use` runs after the tool executor returns. It receives the tool name, input, output when it fits the payload cap, and success status. Hook failures do not alter the tool result.
- `session_start` runs once when a session actor accepts its first Agent run. If Haven is restarted and rebuilds the actor while resuming a session, the event runs again.
- `run_start` runs at the beginning of every Agent run, including resumes and continuations.
- `run_end` runs after each Agent run returns, including completed, paused, cancelled, and error outcomes. It includes a content-free `run_outcome` value and a success flag.
- `pre_compact` runs after the context threshold is crossed and immediately before compaction starts. It includes the estimated token count before compaction. This is a notification; it cannot cancel compaction.
- `post_compact` runs after the compacted transcript has been committed. It includes token estimates before and after compaction.

`pre_tool_use` and `post_tool_use` payloads include tool input; `post_tool_use` also includes the result when it fits. Treat these values as potentially sensitive workspace data. Payloads and each stdout/stderr stream are capped at 64 KiB. If a notification payload is too large, Haven omits tool output and then input and sets `payload_truncated`; an oversized `pre_tool_use` payload fails closed. Raw stdout/stderr and Hook denial text are not inserted into the model transcript.

## Failure behavior and diagnostics

`pre_tool_use` fails closed: if a matching hook cannot run, exits unsuccessfully, times out, is cancelled, is rejected by policy, or returns invalid JSON, the tool does not run. Other events are notifications: a Hook failure is logged as a static error code and does not change the primary operation. A user Hook failure does not prevent later matching notification hooks from being attempted unless the event deadline is exhausted.

The Settings diagnostics panel keeps bounded, in-memory counters per event: event runs, command runs, failures, denials, last outcome, and a static failure code. It never stores executable paths, arguments, payloads, stdout, stderr, or denial text. Counters reset when Haven exits.

Diagnostics distinguish a failed `pre_tool_use` (tool blocked) from a failed notification (primary operation continues), and show a suggested next step for static failure codes: executable/script paths, policy changes, invalid stdout, event time budget, output cap, project configuration and workspace availability. The denial counter counts explicit script denials; failures are counted separately. While an event is in progress its last outcome is unset.

## Execution boundary

Hook commands run only when the active sandbox is `FullAccess` and network policy is `Open`. Haven does not run hooks in `WorkspaceWrite`, `ReadOnly`, `Ask`, `Restricted`, or `Deny` modes because process containment alone cannot restrict a command's filesystem or network access. If a configured `pre_tool_use` cannot run, Haven denies the tool call. Notification hooks are skipped and logged without changing the primary operation.

The command inherits only a minimal runtime environment (`PATH`, Windows system/temp variables), runs with the execution workspace as its current directory, has a five-second event budget, and is attached to a kill-on-close process Job. Cancellation or a security-policy change terminates the process tree. A Hook command still runs with the Haven user's OS privileges; configure only commands you trust, and trust a project workspace only when its hook file and scripts are under your control.
