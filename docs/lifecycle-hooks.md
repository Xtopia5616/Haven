# Lifecycle hooks

Haven can run configured commands at four Agent lifecycle points: `pre_tool_use`, `post_tool_use`, `session_start`, and `pre_compact`. Hook commands are configured in `config.toml`; project hooks are read from `.haven/hooks.toml` only for a workspace explicitly listed in `trusted_hook_workspaces`.

## User hooks

Add commands to Haven's `config.toml`. Commands are launched directly with literal arguments; Haven does not invoke a shell or expand variables in the command string.

```toml
[[hooks.pre_tool_use]]
command = "pwsh.exe"
args = ["-NoProfile", "-File", "C:/Users/me/haven-hooks/check-tool.ps1"]
matcher = "shell"

[[hooks.post_tool_use]]
command = "C:/Tools/haven-audit.exe"
args = ["--record-tool"]
```

`matcher` is optional. Omit it or use `"*"` to match every tool; otherwise it must exactly match a tool name such as `shell` or `files.read`.

User-level hooks and trusted workspace roots are loaded from `config.toml` at Haven startup, so changes to those entries take effect after restarting Haven.

## Project hooks

First trust the exact execution workspace in `config.toml`:

```toml
trusted_hook_workspaces = ["D:/src/my-project"]
```

Then add `.haven/hooks.toml` in that workspace:

```toml
[hooks]

[[hooks.pre_tool_use]]
command = "pwsh.exe"
args = ["-NoProfile", "-File", "scripts/check-tool.ps1"]
matcher = "shell"

[[hooks.session_start]]
command = "C:/Tools/project-session-note.exe"
```

Haven reads the project file through its pinned workspace directory handle and rejects reparse points, non-regular files, invalid TOML, and files larger than 64 KiB. An untrusted workspace's `.haven/hooks.toml` is ignored without reading it. Settings IPC cannot add or remove trusted roots.

## Event behavior

- `pre_tool_use` runs after Haven's normal tool authorization and confirmation, immediately before tool execution. A command can allow or deny the call by writing `{"decision":"allow"}` or `{"decision":"deny","reason":"..."}` to stdout. Empty stdout means allow; a nonzero exit, invalid reply, timeout, cancellation, or security-policy rejection denies the call. Allow never bypasses Haven's authorization.
- `post_tool_use` runs after the tool executor returns. It receives the tool name, input, output when it fits the payload cap, and success status. Hook failures do not alter the tool result.
- `session_start` runs each time a ReAct run starts, including resume and continuation. The payload includes `run_id` and `start_step`.
- `pre_compact` runs after the context threshold is crossed and immediately before compaction starts. It includes the estimated token count before compaction.

Each event is JSON on stdin. `pre_tool_use` and `post_tool_use` payloads include the tool input; `post_tool_use` also includes the result when it fits. Treat those values as potentially sensitive workspace data. Payloads and each stdout/stderr stream are capped at 64 KiB. If a notification payload is too large, Haven omits tool output and then input and sets `payload_truncated`; an oversized `pre_tool_use` payload fails closed. Raw stdout/stderr are not inserted into the model transcript. A clipped `pre_tool_use` denial reason is returned as the tool failure.

## Execution boundary

Hook commands run only when the active sandbox is `FullAccess` and network policy is `Open`. Haven does not run hooks in `WorkspaceWrite`, `ReadOnly`, `Ask`, `Restricted`, or `Deny` modes because process containment alone cannot restrict a command's filesystem or network access. If a configured `pre_tool_use` cannot run, Haven denies the tool call. Notification hooks are skipped and logged without changing the primary operation.

The command inherits only a minimal runtime environment (`PATH`, Windows system/temp variables), runs with the execution workspace as its current directory, has a five-second event budget, and is attached to a kill-on-close process Job. Cancellation or a security-policy change terminates the process tree. A Hook command still runs with the Haven user's OS privileges; configure only commands you trust, and trust a project workspace only when its hook file and scripts are under your control.
