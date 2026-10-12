# ADR 1080：约束持久 ToolRun kind

## 状态

已接受（2026-10-13）

## 背景

`ToolRunKind` 是封闭枚举，只包含 `background` 与 `scheduled`。Tools 的读取投影会拒绝未知 kind，但此前 SQLite `tool_runs.kind` 只要求非空，仍允许写入第三种值；依 kind 筛选的恢复、计数和清理路径会忽略这类行，使数据库中的持久事实与运行时概念分裂。

## 决定

- `tool_runs.kind` 使用 `CHECK(kind IN ('background','scheduled'))`，由 SQLite 与 Rust enum 共同封闭值域。
- 将当前 schema 版本从 40 提升为 41。项目不执行旧版运行时迁移；v40 数据库必须按发布与重置规则删除并重建。
- 新增 schema 回归测试，验证未知 kind 无法持久化。

## 影响

不会改变有效 ToolRun、IPC 或事件字段。直接写入未知 kind 的数据库操作将失败。重置 v40 数据库会清除本地会话、记忆、工具运行和用量；操作说明见 [`release-and-reset.md`](../release-and-reset.md)。

## 验证

- `cargo test --locked -p haven-memory tool_run_kind_is_a_closed_persisted_domain`
- workspace locked tests、strict Clippy、格式检查及 ADR/IPC 校验。

## 回滚

恢复 schema v40 二进制只能打开 v40 数据库或备份。schema v41 数据库不得由旧二进制打开；没有匹配备份时，按项目数据重置流程创建新库。
