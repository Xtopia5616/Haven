# Lifecycle hooks

Haven can run user-configured commands at eleven lifecycle points: `pre_tool_use`, `permission_request`, `post_tool_use`, `session_start`, `run_start`, `run_end`, `session_end`, `toolrun_start`, `toolrun_end`, `pre_compact`, and `post_compact`. User hooks are managed in Settings → Lifecycle Hooks and saved in `config.toml`. The service reads the current ConfigService snapshot for every event, so edits take effect without restarting Haven.

## User hooks

Commands start directly with literal arguments. Haven does not invoke a shell, expand variables, or interpolate event values into the command line. Event data is sent as one JSON object on stdin.

```toml
[[hooks.pre_tool_use]]
command = "pwsh.exe"
enabled = true # default; set false to pause without removing the command
args = ["-NoProfile", "-File", "C:/Users/me/haven-hooks/check-tool.ps1"]
matcher = "shell"

[[hooks.permission_request]]
command = "pwsh.exe"
args = ["-NoProfile", "-File", "scripts/check-confirmation.ps1"]
matcher = "shell"

[[hooks.run_start]]
command = "C:/Tools/haven-session-note.exe"

[[hooks.session_end]]
command = "C:/Tools/haven-session-cleanup.exe"

[[hooks.toolrun_end]]
command = "C:/Tools/haven-background-task-note.exe"
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

Each script reads the JSON object to EOF. These minimal starters allow tool calls; replace the decision with your own checks. For `permission_request`, `allow` means Haven continues to show its normal confirmation prompt; it never approves the operation. Notification events ignore stdout. Keep scripts short: all commands for one event share the five-second budget. Debug messages belong on stderr because `pre_tool_use` and `permission_request` parse stdout as their decision.

PowerShell (`scripts/hook.ps1`, launched with `pwsh.exe -NoProfile -NonInteractive -File scripts/hook.ps1`):

```powershell
$ErrorActionPreference = 'Stop'
$eventData = [Console]::In.ReadToEnd() | ConvertFrom-Json
if ($eventData.event -in @('pre_tool_use', 'permission_request')) {
    @{ decision = 'allow' } | ConvertTo-Json -Compress
}
```

Python (`scripts/hook.py`, launched with `python.exe scripts/hook.py`):

```python
import json
import sys

event = json.load(sys.stdin)
if event['event'] in ('pre_tool_use', 'permission_request'):
    print(json.dumps({'decision': 'allow'}))
```

Node.js (`scripts/hook.mjs`, launched with `node.exe scripts/hook.mjs`):

```javascript
let input = '';
for await (const chunk of process.stdin) input += chunk;
const event = JSON.parse(input);
if (event.event === 'pre_tool_use' || event.event === 'permission_request') {
  process.stdout.write(JSON.stringify({ decision: 'allow' }));
}
```

Use `post_tool_use` for local audit checks based on `success`, `run_end` for per-run integrations based on `run_outcome`, `session_end` for final session cleanup, `toolrun_start` and `toolrun_end` for detached background-task lifecycle integrations, and `post_compact` for compaction metrics. ToolRun hooks receive `tool_run_id`, `tool_name`, `source_step_id`, and `tool_run_status`; `toolrun_end` also receives `success`. They do not receive raw tool input, output, or error text. Notification hooks cannot change Haven state through their response. For example, inspect `event.run_outcome === 'completed'` in a `run_end` script before updating a local completion indicator. Never insert raw tool input into executable code or shell command strings.

User-level command lists run before project-level lists for each event. Within each list, commands run in their configured order. `pre_tool_use` and `permission_request` stop at the first deny or failure. Notification events attempt each configured command in order, subject to the event-wide five-second limit.

## Project hooks and trust

Select a local directory from Settings → Lifecycle Hooks → Project Hook Trust. Haven stores its canonical path in `trusted_hook_workspaces`. Only the exact active execution workspace root can contribute its `.haven/hooks.toml`; a parent/child directory match does not grant trust. Trust can be revoked in Settings at any time.

```toml
[hooks]

[[hooks.pre_tool_use]]
command = "pwsh.exe"
args = ["-NoProfile", "-File", "scripts/check-tool.ps1"]
matcher = "shell"

[[hooks.permission_request]]
command = "pwsh.exe"
args = ["-NoProfile", "-File", "scripts/check-confirmation.ps1"]
matcher = "shell"

[[hooks.post_compact]]
command = "C:/Tools/project-compact-note.exe"

[[hooks.session_end]]
command = "C:/Tools/project-session-cleanup.exe"

[[hooks.toolrun_end]]
command = "C:/Tools/project-background-task-note.exe"
```

Haven reads the project file through its pinned workspace directory handle and rejects reparse points, non-regular files, invalid TOML, and files larger than 64 KiB. An untrusted workspace's `.haven/hooks.toml` is ignored without reading it. Trust authorizes the project Hook configuration only; every resulting command still passes Haven's active security policy.

## Event behavior

- `pre_tool_use` runs after Haven's normal authorization and confirmation, immediately before tool execution. A command can allow or deny the call by writing `{"decision":"allow"}` or `{"decision":"deny","reason":"..."}` to stdout. Empty stdout means allow; a nonzero exit, invalid reply, timeout, cancellation, or security-policy rejection denies the call. Allow never bypasses Haven's authorization.
- `permission_request` runs only after Haven's authorization policy says an operation requires confirmation, immediately before Haven creates the user confirmation prompt. It receives the tool name, tool input, session and run identifiers, and `step_number`. A deny reply or any Hook failure blocks the operation before a prompt appears. An allow reply or empty stdout continues to Haven's normal confirmation prompt and cannot approve the operation.
- `post_tool_use` runs after the tool executor returns. It receives the tool name, input, output when it fits the payload cap, and success status. Hook failures do not alter the tool result.
- `session_start` runs once when a session actor accepts its first Agent run. If Haven is restarted and rebuilds the actor while resuming a session, the event runs again.
- `run_start` runs at the beginning of every Agent run and includes `run_source`: `new_input` for a run queued by new user/session input, `continue` for an explicit continuation or interaction/rollback resume, and `recovery` for a pending run restored at app startup. The source is process-local dispatch metadata; it is not written to the session event log or database.
- `run_end` runs after each Agent run returns, including completed, paused, cancelled, and error outcomes. It includes a content-free `run_outcome` value and a success flag.
- `session_end` runs after an explicit session end has durably changed the session status to `completed`, before terminal cleanup. It runs once for that transition; repeated end requests do not run it again. Deleting a session does not emit this event. It includes the `session_id` and `success: true`; use `run_end` for per-run completion, pause, cancellation, and error handling. When ending an active session, `session_end` can run before that run's eventual `run_end` while the run unwinds. The event is best-effort: a process crash after the status commit and before Hook dispatch can skip it.
- `toolrun_start` runs after the background ToolRun's running row is persisted and its owner session is durably bound. It includes the stable `tool_run_id`, owner `session_id`, `source_step_id` when present, tool name, and `tool_run_status: "running"`.
- `toolrun_end` runs after the ToolRun's terminal state is persisted. It includes the same identity fields, terminal `tool_run_status`, and a `success` flag that is true only for `completed`. If a ToolRun finishes before its owner session is bound, Haven dispatches start then end after the binding commits. A single app-owned worker processes ToolRun Hook batches sequentially from a 32-entry bounded queue; if that queue is full, the new batch is dropped and logged. App shutdown cancels an in-flight Hook. These are best-effort notifications: a process crash between persistence and Hook dispatch can skip one. They do not include raw invocation input, output, or error text.
- `pre_compact` runs after the context threshold is crossed and immediately before compaction starts. It includes the estimated token count before compaction. This is a notification; it cannot cancel compaction.
- `post_compact` runs after the compacted transcript has been committed. It includes token estimates before and after compaction.

`pre_tool_use`, `permission_request`, and `post_tool_use` payloads include tool input; `post_tool_use` also includes the result when it fits. Treat these values as potentially sensitive workspace data. Payloads and each stdout/stderr stream are capped at 64 KiB. All matching commands for one event receive the same bounded JSON. If a notification payload is too large, Haven omits tool output and then input and sets `payload_truncated`; an oversized veto payload fails closed. Raw stdout/stderr and Hook denial text are not inserted into the model transcript.

## Failure behavior and diagnostics

`pre_tool_use` and `permission_request` fail closed: if a matching hook cannot run, exits unsuccessfully, times out, is cancelled, is rejected by policy, or returns invalid JSON, the operation is blocked. Other events are notifications: a Hook failure is logged as a static error code and does not change the primary operation. A user Hook failure does not prevent later matching notification hooks from being attempted unless the event deadline is exhausted.

The Settings diagnostics panel keeps bounded, in-memory counters per event: event runs, command attempts, processes successfully started inside the required execution boundary, failures, denials, last outcome, and a static failure code. When an individual command fails or explicitly denies a tool, diagnostics also show whether it came from the user or project list and its list position. They never store executable paths, arguments, payloads, stdout, stderr, or denial text. Counters reset when Haven exits.

Diagnostics distinguish failed veto events (operation blocked) from failed notifications (primary operation continues), and show a suggested next step for static failure codes: executable/script paths, policy changes, invalid stdout, event time budget, output cap, project configuration and workspace availability. The denial counter counts explicit script denials; failures are counted separately. While an event is in progress its last outcome is unset.

## Execution boundary

In `FullAccess`, Hook commands require `Open` network policy with no domain allowlist and run with the Haven user's OS privileges. A domain allowlist cannot constrain arbitrary child processes, so Hooks fail closed while it is configured. In `WorkspaceWrite`, Windows starts them through MXC with writes restricted by the active execution workspace and Hook file policy; networking is disabled. The command and any script it loads must be readable within the workspace or an allowed runtime directory. To use an external script, add its parent directory under Settings → Security → System and toolchain read-only directories; global path scopes and deny rules still apply. `ReadOnly` blocks Hook processes, and `FullAccess` with `Ask`, `Restricted`, or `Deny` network policy also blocks them. WorkspaceWrite hooks are refused on platforms without Windows MXC, and Haven never falls back to a host process when isolation is unavailable. If a configured `pre_tool_use` or `permission_request` cannot run, Haven blocks the operation. Notification hooks are skipped and logged without changing the primary operation.

The command receives only a minimal runtime environment (`PATH`, `SYSTEMROOT`, `COMSPEC`, `TEMP`, and `TMP`). Under WorkspaceWrite, `TEMP`, `TMP`, and `TMPDIR` point to the execution workspace so temporary writes stay inside the MXC grant. The command runs with the execution workspace as its current directory, has a five-second event budget, and is attached to a kill-on-close process Job or MXC process boundary. Cancellation or a security-policy change terminates the process tree. Configure only commands you trust, and trust a project workspace only when its hook file and scripts are under your control.
