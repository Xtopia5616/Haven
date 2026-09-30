# ADR 0418：Provider 缓存 usage 诊断与 UI 展示

- 状态：Accepted
- 日期：2026-09-30
- 范围：`haven-llm` 缓存诊断、会话用量事件与 Token 明细

## 背景

缓存 token 计数为零可能表示 provider 明确报告未命中，也可能表示响应没有缓存用量字段。将两种情况合并会让排障误判；请求中的缓存策略也无法说明 provider 是否接受了缓存 key。

## 决定

1. `CacheDiagnostics` 记录 provider、有效缓存模式、是否降级、命中/未命中/未知结果，以及 usage 来源。`usage_source=provider` 表示响应中至少出现一个缓存用量字段（包括明确的零）；`unavailable` 表示响应没有提供这些字段。缺少读取计数时保持 `outcome=unknown`，不推断为未命中。
2. 所有 provider adapter 在请求诊断中携带配置的 provider 标识。OpenAI-compatible provider 保留其配置标识；key 被拒绝后诊断反映实际重试模式、`downgraded=true` 与 `key_requested=false`。
3. 诊断随单次 LLM usage 持久化并通过用量事件传递。会话顶部 Token 明细展示模型、provider、缓存策略/结果、用量来源和降级状态；计数未知时不显示为零命中率。
4. 新字段为可选/有默认值的加法契约；旧 usage 记录没有诊断时继续可读。无数据库 schema 变化，也不要求清理用户数据。

## 替代方案

- 将缺少缓存字段按零处理：会把“不知道”误显示为“未命中”。
- 只写入日志：会话级排障需要用户手动关联日志，且恢复历史时不可见。

## 影响与验证

回归覆盖缓存字段缺失、明确零值、provider 只报告写入计数，以及 OpenAI Chat Completions / Responses key 降级诊断。UI 测试覆盖诊断字段呈现和未知语义。

验证通过：

```text
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --locked -- -D warnings
cd ui && corepack pnpm run check && corepack pnpm run test:run && corepack pnpm run build
```

## 回滚

回退本 ADR 对应 adapter、用量 DTO/UI 与测试即可。新增 usage 诊断是可选元数据，不涉及数据库迁移或数据重置。
