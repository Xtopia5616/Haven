# 0925：统一 LLM crate 的跨层 API 导入路径

## 状态

已接受并实现（2026-10-11）。

## 背景

`haven-llm` 已从 crate root 导出 `LlmRouter`、`LlmClient`、大多数请求/响应 DTO、registry、STT 等消费者契约，但仍把 `adapters`、`client`、`media`、`registry`、`router`、`stt`、`types` 等实现模块声明为公开。Agent、Tools、App 和测试因此可以用根 re-export 或实现模块路径导入同一项，例如 `haven_llm::LlmRouter` 与 `haven_llm::router::LlmRouter`。部分真实消费的操作（如 provider `api_style` 投影、health check DTO、media-plan 投影）也因此绕过 crate root 的契约清单。

重复路径让源码组织变成外部依赖；移动文件或重组 provider 实现会影响业务 crate，同时让同一概念在代码审查中出现两种规范拼法。

## 决定

- `haven-llm` 的 provider、client、router、registry、media、STT/TTS、OCR、image generation、stream rules 和 DTO 实现模块均设为私有。
- 将 workspace 真实消费者需要的 DTO 和操作从 crate root 完整 re-export，包括 `EmbeddingRequest`、`HealthCheckRequest`、`Embedding`、web-search phase/update、`api_style_for`、`resolve_web_search_mode` 与 `project_media_plan`。
- provider adapter 具体类型不从 crate root 导出；Agent title 测试改用其已有 mock 填充 router slots。LLM schema sanitizer 留在 crate 内部。
- Agent、Tools、App 与 Rust tests 统一使用 crate root 路径；crate 内部实现仍使用 `crate::...` 模块路径。
- 不再保留同一类型或函数的 module-path compatibility re-export；未被 workspace 消费的实现 helper 随模块一起从外部 API 隐去。移除只被测试调用且没有 workspace 消费者的 `AssemblyAiAdapter::new`。
- 删除只被单元测试调用的公开 `retry_delay` wrapper；测试直接验证已被生产重试路径使用的 `BackoffPolicy`，避免为旧 module API 留存无生产调用的 helper。

## 影响

这是测试版本内的 Rust API 收口，可能破坏 workspace 外部代码依赖的模块路径；本产品不承诺该未发布 API 的向下兼容。Haven workspace 的实际消费者均迁移到 root contract。provider 请求、路由、错误、IPC、配置和持久化行为不变；无数据迁移或重置要求。

## 验证

- 静态搜索确认其他 crate 不再通过 `haven_llm::{adapters,client,image_gen,media,ocr,registry,router,stt,stream_rules,tts,types}::` 访问 LLM 实现模块。
- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked -- --test-threads=1`
- `cargo clippy --workspace --locked -- -D warnings`
- ADR 索引检查。

## 替代方案

- 仅在调用方代码规范中要求使用 root re-export、保留 `pub mod`：拒绝，编译器仍接受第二路径，文件结构继续成为下游契约。
- 每类 API 另加一个公开 namespace façade：拒绝，当前 crate root 已是现有稳定契约面；平行命名空间会再创建第二入口。
- 把所有内部类型继续导出到 crate root：拒绝；仅重导出 workspace consumer 和公开方法签名所需的稳定 DTO/操作。

## 回滚

恢复 LLM 源模块的 `pub mod`、旧 re-export 清单与 workspace 中的 module-path imports 即可。没有持久化或 wire 变化。
