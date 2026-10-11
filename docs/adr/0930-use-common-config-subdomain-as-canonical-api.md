# 0930：统一 Common config 子域 API 入口

## 状态

已接受并实现（2026-10-11）。

## 背景

`haven-common::config` 是配置 DTO、加载与服务操作的领域模块，但 Common root 额外 re-export 了 config 中部分类型和 helper。仓内消费者混用两种写法：`ConfigLoader` 同时以根路径和 config 路径导入；`McpDiscoveryConfig` 在 MCP manager 内也有两种路径；`McpServerConfig` 在 MCP、Tools、App、Agent 混用两种路径；`default_runtime_temp_root` 主要从根路径调用。其它根导出没有独立使用证据。

重复入口没有清晰的公开 API 规则：同一个 config 值有时像 Common 全局契约、有时像配置子域类型。配置专属设置与操作实际都归 `config` 模块。

## 决定

- 以 `haven_common::config` 作为配置类型与 helper 的唯一公共入口。
- 删除 Common root 的 config conveniences re-export；包括 `AppConfig`、`ConfigLoader`、日志类型、MCP 配置、`Settings`、`SkillsExecConfig` 与 `default_runtime_temp_root`。
- 将跨 crate 生产代码与测试里的 root imports/calls 统一迁移到 `haven_common::config::*`。
- `ConfigService` 等仅在 config 模块可用的现有 contract 保持原有 owner；不改配置字段、持久格式或读取行为。

## 影响

这是未发布 workspace API 的破坏性收口，不保留旧 Common root aliases。运行时代码只调整导入路径，TOML、MCP runtime snapshot、目录计算、日志与 Skills 执行配置行为不变。

## 验证

- 静态搜索确认 config 根 re-export 类型没有 `haven_common::<Type>` 使用，配置消费者统一通过 `haven_common::config`。
- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked -- --test-threads=1`
- `cargo clippy --workspace --locked -- -D warnings`
- ADR 索引检查。

## 替代方案

- 保留根路径作为便利 alias：拒绝，同一 config 类型继续有两条规范性无法区分的入口。
- 将整个 config 模块隐藏并扁平导出：拒绝，ConfigService、CredentialStore 等配置领域操作需要稳定的命名子域。
- 只迁移生产调用而保留测试 root paths：拒绝，测试会继续教会后续代码混用入口。

## 回滚

如未来 API 设计证明 config DTO 是 Common 全局无域名的共享值，再单独评估哪些类型确实应离开 config 子域；不要恢复一个同时保留模块路径的重复 alias。
