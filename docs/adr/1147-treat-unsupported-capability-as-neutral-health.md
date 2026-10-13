# 1147：将不支持的能力视为中性健康结果

## 状态

已采纳并实施。

## 背景

LLM Router 对 native transcription 记录请求结果后，才把 `UnsupportedCapability` 交给 audio-chat fallback。通用 outcome projector 将所有非 rate-limit 错误映射为 endpoint failure；OpenAI audio-capable chat model 因非 Whisper 模型不支持 native transcription，成功 fallback 仍会累计 circuit failure 并可能打开 breaker。

## 决定

- `RequestOutcome` 增加 `Neutral`，`UnsupportedCapability` 不修改 endpoint circuit 或 rate-limit cooldown。
- 真正的 provider/network/request failure 仍计入 breaker；成功仍重置连续失败；rate limit 仍同时计入失败和 cooldown。
- capability fallback 在选择支持该能力的后续路由前，不惩罚原 endpoint 健康状态。

## 影响

连续的 capability mismatch 不会把可用 endpoint 标为故障；其它请求种类共用的 outcome projector 也获得一致语义。无配置或持久化变化。

## 验证

新增 outcome 回归：已有一次真实失败后记录 `UnsupportedCapability`，断路器连续失败数保持不变。运行 LLM/workspace 测试和严格 Clippy。

## 回滚

无持久数据重置要求。回滚前须保证能力回退不再将合法 unsupported 结果作为服务故障计数。
