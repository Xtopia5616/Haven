# ADR 0545：将 ToolBox 重命名为 ToolHandle

## 状态

已采纳并实施；Rust workspace 门禁通过。

## 背景

`haven_tools::ToolBox` 实际定义为 `Arc<dyn Tool>`，在 Tools registry、catalog、execution、authorization 和 Agent/App 调用处表示单个共享的类型擦除工具实例。名称 `ToolBox` 容易被理解为工具集合，也像 Rust 的 `Box<dyn Tool>`，两者都没有表达真实类型和引用生命周期。调用方 clone 该值时增加 `Arc` 强引用，底层实现会存活到最后一个引用释放。

## 决定

1. 将公开类型别名 `ToolBox` 改为 `ToolHandle`，在 `haven_tools` crate root 导出新名称。
2. 全部 Rust 使用点（Tools、Agent、App 与集成测试）使用唯一规范名，不保留兼容别名。
3. 在 `docs/naming.md` 记录 `Handle` 表示单个资源的共享引用，不表示集合或生命周期管理器。

## 替代方案

- 改叫 `SharedTool`：拒绝。此名能表达共享，但不能突出它是调用端传递、注册与查找的单值引用。
- 保留 `ToolBox` 并仅补注释：拒绝。代码搜索、类型提示和跨 crate API 仍持续暴露易误读的名称。
- 用 `Box<dyn Tool>` 替换：拒绝。Registry、Agent 和 App 会共享实现，改为独占盒会改变所有权和生命周期语义。

## 影响与验证

- Rust public symbol 名称变化，仅在 workspace 内迁移。底层仍是 `Arc<dyn Tool>`；注册去重、session overlay、授权判断、工具执行和释放时序不变。无 IPC、Serde、数据库或用户数据变化。
- 验证通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked -- --test-threads=1`、`scripts/check-adr-index.ps1` 与 `git diff --check`。

## 回滚

恢复 `ToolBox` 类型别名、Tools crate re-export 和对应 Rust 使用点。无需配置、数据库、IPC 或用户数据迁移。
