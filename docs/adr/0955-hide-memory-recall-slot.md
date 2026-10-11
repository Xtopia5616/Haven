# 0955：将 MemoryTool recall slot 留在 Tools 内部

## 状态

已接受并实现（2026-10-11）。

## 背景

App composition 通过 `StartupWiring.memory_recall: Arc<dyn MemoryRecallPort>` 注入 Agent 的 `MemoryService`；它不需要也不持有 Tools 内部的绑定 slot。实际的 `MemoryRecallSlot = Arc<OnceLock<Arc<dyn MemoryRecallPort>>>` 只由 Tools 创建、在 startup 中绑定，并传给 Tools 自己构造的 `MemoryTool`。全仓没有跨 crate 对 slot alias 或 `new_memory_recall_slot` 的引用。

此前 slot alias 经 Tools crate root 公开，泄漏了 `OnceLock` 绑定实现。`MemoryTool::new` 也仅由 Tools 内部 builtin assembly 与单元测试调用，却公开接受该实现类型。

## 决定

- 将 `MemoryRecallSlot` 与 `new_memory_recall_slot` 限制为 Tools crate 内可见，并从 Tools 的默认 crate-root API 移除 slot alias。
- 将 `MemoryTool` 与其构造器限制在 builtin assembly 的父模块范围内；Tools 内部测试仍覆盖 recall 正常绑定与未配置失败路径。
- 保留跨 crate 的领域接口 `MemoryRecallPort` 和 composition 输入 `StartupWiring.memory_recall`，不改变启动时先绑定再发布工具目录的顺序。

## 影响

App、Agent 继续通过 `MemoryRecallPort` 提供/消费 recall capability；绑定状态与 `OnceLock` 类型只在 Tools 内部。Memory 查询、错误、数据、IPC 和安全语义不变，无需数据重置。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `git diff --cached --check`
- 全仓确认只有 Tools 内部引用 `MemoryRecallSlot` 与 `new_memory_recall_slot`。

## 回滚

若未来确有其他 crate 需要注入或共享 recall provider，应暴露领域 port 操作或由 Tools 提供具名注入入口；不重新导出 `OnceLock` alias。
