# ADR 0060：工具执行结果与幂等重试边界

## 背景

工具执行此前用 `RiskLevel` 间接决定是否重试，并用字符串错误表示取消和超时。
这会把安全确认和执行语义耦合起来，也无法区分“请求没有完成”和“外部副作用可能
已经发生”。此外，`ToolConfig` 的默认 30 秒会覆盖工具自己的操作级超时。

## 决定

- `Tool::idempotency(input)` 是重试的唯一资格判断，取值为
  `Idempotent`、`NonIdempotent` 或 `Unknown`；默认 `Unknown`。只有幂等操作才允许
  使用有限的 transient retry budget，风险等级只负责安全门禁。
- `ToolResult.outcome` 明确表示 `Succeeded`、`Failed`、`Cancelled`、
  `TimedOutAndTerminated` 或 `TimedOutUnknown`，并记录 `attempts`。未知终止结果永不
  自动重试。
- `execute_with_timeout` 在取消/超时时返回结构化结果，并向执行体传播子取消令牌。
  HTTP 请求可以确认请求 future 已终止；Shell、Skill 和 MCP 涉及子进程或远端服务，
  保守标记为 `TimedOutUnknown`，不重新执行命令、脚本或远端调用。
- HTTP GET 声明幂等，POST 声明非幂等；跨会话消息的发送、回复、请求和 spawn 均为
  非幂等。请求等待超时表示消息已投递但回复未知，不能重发。
- `ToolConfig.timeout_secs`、`max_retries` 和 `retry_backoff_secs` 均改为可选覆盖；
  缺省值保留工具 intrinsic policy，`Some(0)` 明确关闭重试。旧的 `retry_unsafe`
  配置字段删除，需删除旧配置中的该字段或按当前配置重新保存。

## 影响与重置

这是工具执行与配置契约的破坏性调整，不修改数据库。带有旧
`tool_settings.*.retry_unsafe` 的配置需要删除该字段；只配置 enabled、输出上限、路径
或 timeout 的工具设置不再改变工具 intrinsic retry policy。

## 验证

覆盖结构化取消/超时、幂等 GET 与非幂等 POST、消息请求超时、Shell/Skill/MCP 的
未知终止、attempt 可观测性和“只配置其他字段不覆盖 timeout”的回归测试；执行
`cargo fmt --all -- --check`、严格 Clippy、workspace Rust tests。

## 回滚

回退本 ADR 对应提交即可恢复旧的工具结果与配置字段；若用户配置已保存，需要手动
恢复 `timeout_secs` 的数值字段并重新启动 Haven。
