# ADR 0120：能力驱动的多模态 role fallback 与 wire 回归

日期：2026-09-10
状态：已采纳（阶段 4 多模态路由补强切片）
关联：[ADR 0113：统一多模态资产、表示与请求投影](0113-unified-media-asset-representation-projection.md)

## 背景

ReAct 请求原先先按内容 modality 选择 `vision` / `audio` role，再只针对该
adapter 做 media projection。专用 role 虽然已配置，但若其 capability profile
拒绝实际 MIME、单请求大小或 part 数，planner 会直接降级成占位文本；即使
default role 可以原样承载请求，也不会尝试 default。另一个问题是逐附件规划
绕过了 profile 的请求级 aggregate limits。

## 决定

- role 选择接收完整的 provider request context。专用 role 必须通过所有当前
  raw image/audio parts 的 MIME、大小、part 数和可投影检查；失败时先检查
  `DefaultModel`，只有 default 也不兼容时才保留专用 role 让安全降级继续生效。
- 选定 role 后，对整个 request 一次性执行 media plan，再把结果按原 content
  位置投影；因此 `max_input_parts` / `max_input_bytes` 在最终 wire request
  之前生效，而不是按附件分别重置。
- 每个 provider adapter 的 capability profile 与最终 wire JSON 保持同一组
  回归测试，覆盖 OpenAI Chat、OpenAI Responses、Gemini 和 Anthropic 的图片
  负载形状。

## 安全与可观测性

未知 capability 仍不视为支持；不能安全投影时继续使用显式占位并发送稳定的
`MediaPlanNotice`。role fallback 只改变本次 provider 路由，不修改 durable
canonical transcript，也不会把本机路径放入 provider payload。

## 验证与回滚

测试覆盖专用 role 的 MIME/大小拒绝回落 default、请求级 part 限制，以及四种
provider 的 capability-to-wire 映射。回滚只需恢复原先的 role 选择和逐附件
projection；无需数据库 schema 迁移。
