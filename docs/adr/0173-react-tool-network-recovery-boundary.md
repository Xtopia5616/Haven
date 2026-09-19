# ADR 0173：ReAct、工具与网络请求的失败恢复边界

## 状态

已接受（2026-09-19）

## 背景

ReAct 回合同时跨越 provider 流式请求、工具扩展点、持久化投影和恢复快照。过去某些
失败只返回到最外层 dispatcher：流式输出可能已经到达 UI 但没有恢复标记，工具扩展
panic 可能取消整批并列调用，显式请求策略中的 fallback 也没有真正参与请求。网络错误
body 还可能由 provider 无界地返回，放大单次故障的内存占用。

## 决定

1. `haven-llm` 对同一 `RequestPolicy` 保存有序候选列表。请求只在传输、超时、服务端、
   限流或其他 endpoint 级失败且尚未产生可见流式输出时切换候选；取消、上下文超限、
   内容安全拦截、计费和已产生输出的流式失败直接返回，避免重复执行或语义漂移。
2. chat、stream、embedding 和 native transcription 统一使用请求级 timeout、retry、
   circuit、concurrency permit 和候选切换。转写切到 multimodal chat 时必须先释放
   AudioModel permit，避免同角色嵌套调用死锁。
3. provider 错误响应体读取最多保留 64 KiB；错误日志继续走脱敏路径。配置中的零总超时
   在请求策略边界钳制为至少 1 秒。
4. 工具扩展、MCP 和 skill 执行 panic 转为结构化 `unknown_outcome`，不让一个工具中断
   同批其他调用，也不允许自动把可能已经产生副作用的调用当作可安全重放。
5. ReAct 的 provider fatal、响应策略重试失败和其他回合错误都在退出前保存恢复快照；
   已产生的 partial output 只进入 recovery-only 路径，不伪装成已接受的 transcript。

## 替代方案

- 只在 provider 内部重试：无法覆盖流式首字节前的候选故障、embedding/STT 和工具
  扩展 panic。
- 所有错误都切换 fallback：会掩盖上下文/安全/计费问题，并可能重复带副作用的请求。
- 让 dispatcher 统一补快照：响应周期已经有 partial output 时会覆盖更精确的恢复标记。

## 影响

- 一次逻辑 LLM 请求的候选顺序在开始时固定；热更新不会改变进行中的请求。
- fallback 仍受同一个逻辑请求的总 deadline 约束；已显示的流式内容不重播，用户可从
  durable checkpoint 继续。
- 工具 panic 的 UI/模型观察结果会明确显示未知结果，人工确认或幂等策略可决定后续动作。
- provider 错误诊断更短且有界，不改变 `LlmError` 的公共分类。

## 验证

- `haven-llm` 单元测试覆盖 chat fallback、流式首输出前 fallback、语义错误不切换、
  零 timeout 钳制和请求策略执行。
- 运行 `cargo fmt --all -- --check`、`cargo check --workspace --locked`、相关 crate
  测试、workspace test 和 clippy；失败的 provider/body、工具未知结果和恢复快照路径
  继续通过现有集成测试验证。

## 回滚与重置

回滚时必须同时恢复 router 候选语义、ReAct partial checkpoint 和工具 panic 的结构化
结果映射；不涉及数据库 schema 或持久化格式变更，无需数据重置。已有 recovery-only
快照按旧版本的未知字段兼容规则处理。
