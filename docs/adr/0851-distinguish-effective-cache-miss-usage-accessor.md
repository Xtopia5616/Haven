# ADR 0851：区分 Usage 缓存未命中字段与有效值 accessor

## 状态

已完成（2026-10-10）。

## 背景

`haven-llm::types::Usage` 同时暴露 `cache_miss_tokens: u32` 字段和同名 `cache_miss_tokens()` 方法。字段由 adapters 写入归一化计数；方法在字段非零时返回它，否则依照 `CacheAccounting` 从 prompt、cached 与 cache-creation counts 推导有效值。LLM Router、OpenAI adapters 和 Agent 都调用该方法，因此调用点无法从名称看出它执行了 fallback 计算。

## 决定

1. 保留 `Usage.cache_miss_tokens` 字段名，维持当前 Rust/Serde 数值字段、usage event 和持久化投影。
2. 将计算 accessor 改名为 `Usage::effective_cache_miss_tokens()`，并同步 Router、OpenAI Chat/Responses adapters 与 Agent 调用点。
3. 保持选择规则不变：非零 adapter 归一化值优先；为零时按当前 `CacheAccounting` 分支计算。增加 inclusive 与 exclusive fallback 断言。

## 替代方案

- 把 accessor 继续命名为 `cache_miss_tokens()`：拒绝。该名与直接可读写的同名字段重叠，调用处隐藏了派生行为。
- 改名/改类型或持久字段：拒绝。本切片只消除 accessor 命名歧义；没有证据要求改变 usage 数据契约或零值规则。

## 影响与验证

只调整 workspace 内方法名与文档，数值字段、序列化字段名、事件和 SQLite usage row 不变。验证：`cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、旧 accessor 全仓搜索、ADR 索引/链接检查与 `git diff --check`。

## 回滚

恢复 accessor 原名和各调用点即可；没有数据迁移、配置变更或 IPC 重置。
