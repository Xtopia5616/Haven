# ADR 0616：命名 SessionRunEngine

## 状态

已采纳并实施。

## 背景

`haven_agent::RunEngine` 是公开导出的 session execution boundary：它包装 `RunHandler`，由 supervisor/actor 在 admission 完成后执行一次完整 ReAct run。类型名只描述“run engine”机制，离开 `session` 模块后缺少所属领域；同时它与算法主循环 `ReActEngine` 是两个不同 owner。

## 决定

1. 将 `RunEngine` 重命名为 `SessionRunEngine`，并同步 `session` 模块和 `haven_agent` crate 的公开导出。
2. 保持 handler 字段、构造器、执行方法、admission 和生命周期不变。
3. `ReActEngine` 继续命名 Agent/ReAct 算法循环；`SessionRunEngine` 只表示 supervisor handoff 的单次会话运行边界。

## 替代方案

- 保留 `RunEngine`，依赖 `haven_agent::session` 上下文：拒绝，该类型从 crate root 公开导出，顶层消费者和 review diff 中名字仍过于泛化。
- 改名为 `ReActEngine`：拒绝，`ReActEngine` 拥有算法与 step 控制，而 `SessionRunEngine` 只在 session run 边界执行 handler。
- 改成 `SessionExecutionEngine`：拒绝，项目术语把一次前台执行称为 session run；`run` 也与 supervisor 的 worker loop 和 dispatcher 用语一致。

## 影响与验证

- 这是 `haven-agent` 的 Rust source API rename；workspace 调用方已同步，运行行为、事件、wire 和持久化不变。
- 命名路线图 §5.7 继续保持 Active；其他公开泛名类型与各后缀实例仍需逐域审计。
- 验证：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`、ADR 索引及 staged diff 检查。

## 回滚

恢复 `RunEngine` 并同步其 crate root 与 session module export；无需数据或 wire 迁移。
