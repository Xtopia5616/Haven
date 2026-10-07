# ADR 0686：删除 LLM Router 旧布尔路由入口

## 状态

已采纳并实施。

## 背景

`LlmRouter::force_routing_flags(stt_use_audio_model, vision_use_image_model)` 是早期布尔路由开关 API 的 `#[doc(hidden)]` 公共包装，只转调 `force_request_routes`。后者注释明确称为旧测试保留，按两个 boolean 改写 `AudioChat`、`Transcription` 与 `Vision` request policy。没有生产调用方；两个跨 crate 测试用布尔值选择默认路由模型。当前模型路由由 `RequestKind` 和 request policy 管理。

## 决定

- 测试通过窄 helper `force_request_primary_for_test(RequestKind, model_id)` 设置具体 request policy 的 primary model。
- 删除 `force_routing_flags` 与 `force_request_routes`。
- 保留用于跨 crate 配置检查的 `force_request_configured(RequestKind, bool)`；该 helper 操作一个具体 request policy 的凭据状态，有实际测试消费者，不属于旧路由 flags 接口。

## 替代方案

- 保留旧方法供潜在生产调用者：拒绝。当前没有生产调用方，且保留了已被 request policy 替代的布尔路由词汇。
- 让测试继续使用旧布尔 API：拒绝。两个调用点已改用 `RequestKind` 和 model id 表达具体路由目标，测试 helper 不再模拟多个旧 flags。

## 影响与验证

- 删除 LLM Rust API 中两个无生产调用方的隐藏 public methods，并将两个测试调用迁移到按 `RequestKind` 设置 primary 的窄 helper；不改变生产模型路由、配置、IPC 或持久数据，无需重置。
- 已执行 Rust workspace check、严格 Clippy、格式检查、ADR 索引与差异检查；按本轮工作约束未运行测试。

## 回滚

可恢复被删的两个方法；无数据或配置迁移。
