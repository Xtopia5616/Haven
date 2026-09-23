# ADR 0221：普通与工具 chat 固定 RequestPolicy 快照

- 状态：已采纳（2026-09-24）
- 范围：`haven-llm` router 普通 chat 与 tools-chat 内部执行边界
- 关联：[ADR 0023](0023-llm-request-policy-boundary.md)

## 背景

普通 chat 与 tools-chat 都在持有 request permit 后执行总超时和重试，但此前总超时 helper 与 retry helper 各自读取一次 `RouterConfig`。两次读取之间若配置发生更新，同一 logical request 可能使用不同的 timeout 与 retry 策略。

## 决定

1. 普通 chat 和 tools-chat 共用一个私有执行入口；进入 `with_request_permit` 闭包后，从当前 router config 构造一次 `RequestPolicy`。
2. 将同一个 Copy 快照传给总超时与重试执行器。请求开始后配置更新不会改变该请求的策略。
3. 流式、embedding 与 transcription 路径继续独立捕获和应用各自策略；不改变其执行边界。
4. 保持 permit 持有范围、circuit breaker、rate-limit 冷却、health 统计、provider messages/tools/max_output_tokens、返回 usage、`router` 错误 operation 名称、重试次数与配置热更新边界。

## 替代方案

- 保留两个 helper 各自读取配置：一次请求可能组合出不同的策略快照，拒绝。
- 将策略捕获下沉到 `request_pipeline.rs` 或改变公开 router 方法：会扩大本内部切片边界，拒绝。

## 影响与验证

只调整 `router.rs` 的私有 chat 执行边界并新增本 ADR；公开 API、跨 crate 类型、provider adapter、wire payload、配置 schema 和 usage 契约均不变。普通 chat 与 tools-chat 共用捕获点；窄测试覆盖两种 provider 调用的 output cap 透传及 usage 返回。

验证命令：

```text
cargo fmt -p haven-llm -- --check
cargo check --locked -p haven-llm
cargo test --locked -p haven-llm --lib
cargo clippy --locked -p haven-llm -- -D warnings
git diff --check
```

## 回滚

回退私有 router 执行边界改动并删除本 ADR。没有持久化、配置格式、公开 API 或 provider wire 数据迁移。
