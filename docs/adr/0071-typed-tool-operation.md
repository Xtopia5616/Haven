# ADR 0071：Typed ToolOperation 运行时契约

## 背景

Builtin 工具已经有若干 typed 参数结构，但 Tool 的运行时元数据仍通过
operation 字符串和 serde_json::Value 分散计算。模型工具 schema、风险、
幂等性、并发、超时与真正执行的分支因此可能出现漂移。Self/Admin 的第一条
受限 surface 切片还保留了 SelfOperation/SelfParams 作为迁移期 native facade。

## 决定

1. 在 haven-tools 增加 TypedToolOperation 和 TypedToolAdapter。operation
   自己声明 typed args、typed output、typed error，以及 capability、operation、
   scope、risk、idempotency、cancellation/timeout 和 concurrency metadata。
2. TypedToolAdapter 只在 LLM/provider 边界把 JSON 解析为 typed args，并把 typed
   output 序列化为现有 ToolResult；风险、重试、并发和 timeout 查询全部从同一
   typed operation 的 metadata 派生。解析失败采取保守的 Critical/Unknown/Exclusive
   默认值，并在执行前由既有 schema gate 拒绝。
3. 首条完整切片是 haven_config：config_get 和 logs_level 使用带 serde tag
   的严格变体；日志级别直接使用 LogLevel，写入经过 ConfigService::apply_patch，
   只读结果经过脱敏。读取结果保留原有 provider-facing shape，内部仍有明确的
   ConfigOperationOutput 变体。
4. serde_json::Value 在这条切片只存在于可演进的、只读的配置 projection；稳定
   的 operation selector、日志级别输入和写入 patch 不再用动态 JSON。后续 domain
   operation 必须复制 typed contract，而不是给旧 SelfParams 增加字段。

## 未完成边界

haven_skills、haven_tools、haven_mcp、diagnostics/session diagnostics 以及
Tauri command 目前仍通过 SelfTool/SelfOperation。它们不是新的兼容承诺：
后续切片必须分别迁移到 typed domain operation 后删除 SelfTool、SelfParams、
SelfOperation 和 run_admin_op，不能让本 ADR 的 adapter 成为新的万能 dispatcher。

## 影响与验证

- haven_config 的模型工具名和基本返回 shape 不变；缺少 level、未知字段、
  config_set 和越权 operation 都会在 schema/serde 层拒绝。
- 配置 TOML schema、现有 ConfigService 版本和 Tauri wire payload 不变，不需要
  删除用户配置。
- 验证包括 typed operation metadata、成功持久化、重复幂等、取消无副作用、缺参、
  未知字段、已删除 config_set 和路径不存在测试。

重点命令：

    cargo fmt --all -- --check
    cargo test --locked -p haven-tools admin
    cargo check --locked -p haven-tools -p haven-app-binary
    cargo clippy --workspace --locked -- -D warnings

## 回滚

回退本 ADR 对应提交即可将 haven_config 恢复为旧 capability adapter；不需要
修改 TOML 或数据库。后续 admin domain 迁移应保持单独提交和单独回滚边界。
