# ADR 0436：从可复用提示前缀移除易变时钟

## 状态

已接受（2026-10-03）。

## 背景

DeepSeek 缓存按精确请求前缀自动匹配。Haven 在恢复每轮会话时会重建 system prompt；runtime snapshot 曾包含精确到秒的 `local_time`，而 system prompt 位于持久 transcript 和新输入之前，因此每次恢复都让后续历史无法沿用前一轮的缓存前缀。

本机用量记录提供了相符的证据：一个会话的 34 次模型调用累计 prompt 为 1,082,385 tokens，缓存 477,184 tokens（44.1%）；多轮首个请求反复报告 9,856 个缓存 token，而同一轮后续前缀稳定时达到 97–99%。工具 schema 后续扩张也会改变 provider `tools[]` 前缀，是另一种可观察的缓存断点。

## 决定

1. 从每轮恢复重建的 runtime snapshot 中移除当前日期、时间和时区偏移等易变时钟值。
2. 对当前日期或时间敏感的任务，通过现有 `system.info` 操作的 `category=locale` 按需读取本地时间、UTC 时间和时区。
3. 保留需要热重载后及时更新的 runtime 能力、权限和环境事实；工具 schema 延迟加载并按 ADR 0435 批量加载本轮可预见的能力。

## 替代方案

- 将秒级时间改为分钟或日期：仍会周期性改变前缀，也会让运行中的时间上下文陈旧；不采用。
- 固定整个 runtime snapshot：缓存前缀更稳定，但会向模型暴露热重载前已失效的 shell、MCP、媒体或权限能力；不采用。
- 每轮都附上当前时间：保留最新时间但重置 transcript 后缀缓存；不采用。

## 影响与验证

常规恢复不再因秒级时间变化切断可复用 transcript 前缀。需要精确当前时间的任务多一次按需 `system.info` 能力发现/调用。provider 缓存仍是 best-effort；工具 surface、memory、runtime 配置实际变化也会改变请求，故此改动不保证每次或累计命中率超过 90%。静态检查与门禁尚未运行。

## 回滚

恢复 runtime snapshot 中的时钟字段并删除本 ADR 即可；无数据库、配置或用户数据迁移。
