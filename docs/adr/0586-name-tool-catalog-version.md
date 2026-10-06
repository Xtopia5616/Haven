# ADR 0586：统一 Tools 目录版本时钟类型

## 状态

已采纳并实施。

## 背景

一个 session 的可执行 tool catalog 版本由两个独立时钟组成：全局已安装 catalog 版本和 session-local overlay 版本。Tools 的 `SessionToolOverlay` / `ToolsFacade` 返回 `(u64, u64)`，`ToolCatalogSnapshot::version()` 也暴露相同位置 tuple；Agent 的 provider-definition cache 把该 tuple 作为失效键，并以 `.0` / `.1` 输出诊断字段。相同概念在跨 crate 边界没有具名 owner，使用位置索引掩盖了全局与会话作用域的不同。

## 决定

1. Tools 唯一定义并导出 `ToolCatalogVersion { global_catalog_version, session_overlay_version }`。
2. `SessionToolOverlay::catalog_version_for_session`、`ToolsFacade::catalog_version_for_session` 与 `ToolCatalogSnapshot::catalog_version` 使用该结构；移除 snapshot 的泛化 `version()` getter。
3. Agent cache entry 使用 `catalog_version` 与 `prepared` 具名字段，并用同一 `ToolCatalogVersion` 比较失效条件。
4. 全局目录变更仍只推进 `global_catalog_version`；会话加载/移除仍只推进对应的 `session_overlay_version`。两者都只用于进程内 snapshot/cache，不持久化、不序列化、不进入 IPC。

## 替代方案

- 继续返回 `(u64, u64)` 并依赖文档解释顺序：拒绝，Tools、Facade、Agent cache 和日志仍可能把顺序写反。
- 在 Agent 自己定义另一份版本 DTO：拒绝，版本事实属于 Tools catalog owner，独立副本会造成跨 crate 映射和重复词汇。
- 只重命名 `.0/.1` 局部变量：拒绝，快照公共 Rust API 与缓存仍将两个 clock 编码在 tuple 位置中。

## 影响与验证

- 更新 Tools registry、Facade、snapshot、Agent cache、catalog diagnostics、相关测试和跨层输出契约清单。
- Tool catalog capture/retry、缓存命中/失效与 session isolation 语义不变；无数据库、配置、IPC、事件或用户可见行为变化，无需重置。
- 验证通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked -- --test-threads=1`、ADR 索引及 `git diff --check`。

## 回滚

将 `ToolCatalogVersion` 恢复为 `(u64, u64)`，恢复 snapshot `version()` getter 和 Agent cache tuple entry，并还原相关调用点。该类型不落库或序列化，不需要数据重置。
