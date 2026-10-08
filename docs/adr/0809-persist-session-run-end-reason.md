# ADR 0809：持久化会话运行结束原因

## 状态

已采纳并实施。

## 背景

Session lifecycle event 带有用户可见的结束原因，但该原因此前只存在于当前 UI 进程。应用重启后，历史恢复只能显示通用文案，丢失用户中断、运行错误或显式结束的具体说明。

## 决定

1. 在 sessions.run_end_reason 保存该会话最近一次 paused、completed 或 error 运行的净化后原因。该字段由 SessionStore 持久化，并通过会话历史/恢复 DTO 暴露给 UI。
2. 新一轮进入 pending 或 running 时清空旧原因；新的终止原因在对应 lifecycle event 发布前写入。错误文本仍经过 sanitize_error_text，不进入 transcript、ReAct event replay 或 UI-only message。
3. 缺少具体原因时继续使用现有状态通用文案。Reason 是会话生命周期元数据，不改变 session transcript 的 session_events 权威边界。
4. schema 从 v38 升至 v39。旧数据库不做运行时迁移；升级需按发布与重置说明删除旧数据库。

## 替代方案

- 继续只保留前端进程内缓存：拒绝，无法满足应用重启后的历史恢复。
- 把原因追加到 transcript：拒绝，原因不是对话内容，也不应进入模型恢复上下文。

## 影响与验证

- 持久化影响限于会话表新增 run_end_reason；会话历史、resume DTO 和 Rust→TypeScript IPC 契约同步增加该字段。
- `cargo fmt --all -- --check`、`cargo check --workspace --locked`、严格 Clippy、UI `check` / `build` / Prettier，以及 IPC 生成、契约和事件目录检查均通过。未运行测试。

## 回滚

移除此列和 DTO 字段并回退写入路径。v39 数据库不能由旧 schema 二进制打开；回滚需按发布与重置说明重建数据库。
