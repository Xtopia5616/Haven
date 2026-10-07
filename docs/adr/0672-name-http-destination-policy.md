# ADR 0672：区分 HTTP 目标策略与全局 NetworkPolicy

## 背景

Common `NetworkPolicy` 是应用安全设置使用的全局访问模式（Deny/Ask/Restricted/Open）。`tools::builtin::http` 另有私有 `NetworkPolicy`，它实际只含 HTTP host allowlist 与测试 loopback 开关；该模块的请求方法和输入也分别叫 `NetworkMethod`、`NetworkParams`。两个策略形状和 owner 不同，却共享泛名，调用时容易把目标校验误读成全局授权策略。

## 决定

- HTTP 私有目标约束改名为 `HttpDestinationPolicy`。
- 输入类型改名为 `HttpRequestParams` 与 `HttpRequestMethod`；目标校验和 DNS 解析 helper 改名为 `validate_http_destination` 与 `resolve_http_destination`。
- 删除旧 Rust 符号名，不保留内部 alias。HTTP JSON 参数/结果、Schema、全局 `NetworkPolicy` 安全语义与 SSRF/redirect 规则不变。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --locked -p haven-tools`
- ADR 索引检查与 `git diff --check`
- 未运行测试；本轮只执行格式与编译门禁。

## 回滚与重置

同步恢复 HTTP module 内部类型/helper 名称即可回滚。没有配置、持久化或 wire shape 变化，无需重置。
