# Prompt routing calibration cases

This small corpus checks whether Haven's short capability index helps the model choose among native tools, user-configured Skills, and user-configured MCP servers. It evaluates the first useful routing decision, not answer style or an exact tool-call sequence.

## Run conditions

- Start each case in a fresh session with the listed capability fixtures and the normal Haven prompt/tool surface.
- Keep the provider, model, and capability descriptions fixed when comparing prompt revisions. Record their IDs with the result.
- Observe the discovery/load choice and the final capability used. Do not require identical wording or an identical number of harmless discovery calls.
- A case passes when the chosen capability can complete the request, unrelated extensions are not loaded, and unavailable capabilities are reported accurately.
- Keep this as a manual, provider-specific calibration set. Run it when prompt or catalog guidance changes, or when repeated routing mistakes are reported; it is not a CI gate.

## Cases

| Case | Available capabilities | User request | Expected route | Failure signal |
|---|---|---|---|---|
| Local file search | Native `files.search`; no matching Skill or MCP | “在工作区里找出包含 `TODO` 的 Rust 文件，列出路径和附近内容。” | Discover and load the native file-search operation, then search the workspace. | Loads an unrelated extension or claims to have searched without using a file capability. |
| Specialized workflow | Native file operations; enabled Skill `release-notes` for drafting release notes | “按 release-notes 工作流，为当前改动起草发行说明。” | Load the named Skill and use local files only as needed for source material. | Ignores the requested Skill or loads unrelated MCP servers. |
| Configured integration | MCP server `calendar` exposes `list_events`; not yet loaded | “查一下我明天上午有没有会议。” | Load the relevant Calendar tool and query it. | Fabricates calendar data or treats an unloaded tool as already callable. |
| Omitted index entry | Many enabled Skills; the matching `meeting-brief` entry falls beyond the compact index budget | “按 meeting-brief 工作流准备明天会议的简报。” | Browse the capability catalog for the matching Skill, then load it if available. | Concludes the Skill is absent based only on its omission from the short index. |
| No extension needed | Normal native tool surface; Skills and MCP may also be configured | “把 18 乘以 7。” | Answer directly without loading a Skill or MCP tool. | Expands the tool surface for a task that needs no external capability. |
| Unavailable integration | MCP server `calendar` is configured but currently exposes no callable tools | “查一下我明天上午有没有会议。” | Check the relevant integration; if it remains unavailable, say so without inventing results. | Claims a successful lookup or silently switches to unrelated data. |
| Combined local and remote context | Native `files.read`; MCP server `calendar` exposes `list_events` | “结合明天的日历安排和工作区里的 `plan.md`，给我一份简短日程建议。” | Use Calendar and the local file, loading only the capabilities needed for both sources. | Omits one requested source or loads unrelated Skills/MCP servers. |
| Local source beats generic search | Native `files.search`; an MCP web-search server is also configured | “只在当前工作区搜索 `timeout_ms` 的用法。” | Use local file search; the request explicitly scopes the source to the workspace. | Searches the web or presents web results as local source evidence. |

## Record

For each run, record the date, provider/model, prompt revision, case, first routing decision, loaded capabilities, outcome, and one short note for any failure. Treat a failure as evidence for a targeted change to the relevant capability description or prompt section; do not add a general rule unless multiple cases show the same gap.
