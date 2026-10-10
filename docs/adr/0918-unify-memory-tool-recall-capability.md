# 0918：统一 MemoryTool Recall capability 并移除 fact-store fallback

## 状态

已接受并实现（2026-10-11）。

## 背景

`MemoryTool` 有一条已绑定的 `MemoryRecallPort`，生产 App 将 Agent `MemoryService` 绑定到 Tools runtime，工具目录重建后，recall 会沿 Agent `MemoryService` → Memory `MemoryRecallStore` 的统一路径执行。

但 slot 未绑定时，`MemoryTool` 会退回 `MemoryFactStore::recall_keyword`。这项操作与 `MemoryRecallStore::retrieve(query, None)` 使用同一 `MemoryRetriever`，只因挂在 facts store 上而形成第二个 recall capability。它使 facts persistence port 同时承担 recall，并令未完成组合的 Tools runtime 悄悄启用另一条结果路径。正常启动次序已保证先绑定 `MemoryRecallPort` 再发布工具目录；缺少该 capability 表示组合未完成，应显式失败。

## 决定

- `MemoryTool` 的 Recall 操作只经已绑定的 `MemoryRecallPort`。slot 未绑定时返回“memory recall capability is not configured”，不自行执行另一种检索。
- 删除 `MemoryFactStore::recall_keyword` 及其专属测试。MemoryFactStore 保留事实持久化和事实查询职责；统一检索仍由 `MemoryRecallStore` 提供，再由 Agent `MemoryService` 编排并经 `MemoryRecallPort` 提供给 Tools。
- 不新增 Tools 对 MemoryRecallStore 的第二次注入，也不保留兼容 fallback。测试需要验证 recall 时显式绑定 fake `MemoryRecallPort`；MemoryRecallStore 的 keyword-only retrieval 由其自身测试覆盖。
- `MemoryRecallSlot` 继续由 Tools startup 在 builtin catalog rebuild 前绑定；其余 tool operation 不变。

## 影响

App 正常 startup 已将 Agent `MemoryService` 绑定给 Tools，因此生产 UI/tool recall 使用路径不变。缺少 recall capability 的 headless 或错误组装现在显式失败，不会静默执行 keyword-only 结果。`MemoryFactStore` 不再暴露与 `MemoryRecallStore` 重叠的生产操作。

不改变数据库 schema、持久化数据、Recall IPC payload、prompt recall、事件、安全过滤或工具目录结构，无需数据重置。Rust API 有意删除重复方法，不提供兼容别名。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`
- 全仓确认 `MemoryFactStore::recall_keyword` 无剩余引用，`MemoryTool` recall 仅经 `MemoryRecallPort`。

## 替代方案

- 保留 keyword fallback 并继续依赖 `MemoryFactStore`：拒绝，因为它保留了第二个 recall owner，并掩盖 composition port 未绑定。
- 在 Tools 再注入 `MemoryRecallStore` 作为 fallback：拒绝，因为生产 App 已有唯一的 embedding-aware `MemoryRecallPort`；添加第二条 store path 会继续允许同一工具以不同语义执行 recall。

## 回滚

可恢复 `MemoryFactStore::recall_keyword`、`MemoryTool` 未绑定时的调用与对应测试；无数据或持久化恢复步骤。
