# ADR 1433：恢复 Tools 严格 Clippy 基线

## 状态

已实施 — 2026-10-27。

## 背景与决定

ADR 1432 的依赖检查发现三条既有告警：剪贴板历史公开 `len` 却只在测试中提供 `is_empty`，以及 MCP 配置的 test-support 分支有两个冗余 return。

源码调用和全目标编译复核确认 `ClipboardHistory::len` 由工具结果和 crate 内测试使用，`is_empty` 只有 crate 内测试消费；`coordinator.rs` 的容量更新测试也使用二者。因此将二者收为 crate 内可见，不为消除 lint 增加无生产消费者的跨 crate API，并保留 `is_empty` 的测试限定。历史容量、内容和线程安全保持不变。MCP fixture 分支改为尾表达式，保留生产 ConfigService 优先读取、排序与 revision 绑定。

Tools 全目标检查还暴露了既有测试 lint：测试模块之后的 helper、冗余转换/更新、等值分支和 default 后赋值。将测试模块移至文件末尾并作等价整理；安全夹具继续从 `SecurityConfig::default()` 的全局策略取得剩余字段，不能用不同的 `ResourcePolicy::default()` 替代。HTTP 重定向夹具改为最多 4 KiB 的完整头部读取，正确处理分段与 EOF，避免忽略 read 长度或仅检查部分头部。生产授权、搜索、计划和 Skill 执行逻辑均不改变。

本切片不改变跨 crate 生产契约、IPC、数据库或配置，无需重置；回滚恢复方法可见性、return、测试布局及夹具。剪贴板、HTTP、Shell、安全引擎和 Skill 文件超过职责复核阈值，本次保持已有 owner，不为静态告警启动新的平台/授权职责拆分。

## 替代方案

- 为历史补公开 `is_empty`：扩大没有消费者的生产接口。
- 禁用 lint 或继续绕过依赖：无法恢复原严格门禁。

## 验证

复用已有剪贴板历史、MCP 配置与 Tools 行为测试，不新增仅镜像语法/可见性的测试。Rust 1.99.0，沿用隔离 `CARGO_TARGET_DIR=target/r04-verification` 与 `CARGO_INCREMENTAL=0`。

- `cargo check --locked -p haven-tools`：通过。
- `cargo clippy --locked -p haven-tools --all-targets -- -D warnings`：通过，包含依赖，无告警豁免。
- `cargo test --locked -p haven-tools -- --test-threads=4`：1014 项单元测试、7 项集成测试通过，16 项原有忽略用例保持不变；doc tests 通过。
- 全局安全默认值对应的 runtime read-root 用例另行复跑：1 passed。
- `cargo clippy --locked -p haven-agent --all-targets -- -D warnings`：通过，关闭 ADR 1432 当时的依赖检查限制。
- 格式、差异与 ADR index 检查通过；Everything/Schedule 的模块移位核对测试和 helper 正文未改，Consent 的实现与测试亦仅移位。
- 无 UI、IPC 或跨 crate 生产契约变更，未运行新的前端或 workspace 全量门禁；Windows 发布 Gate 不因此关闭。
