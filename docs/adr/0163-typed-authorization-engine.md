# ADR 0163：AuthorizationEngine 统一 typed 授权模型

- 状态：accepted
- 日期：2026-09-15
- 范围：`haven_common`、`haven_tools`、`haven_agent`、`haven_app-binary`
- supersedes：AuthorizationEngine 的 tuple-style authorization adapter

## 背景

授权入口曾分别传递 session id、tool name、动态输入和 `OperationPolicy`，并在
receipt、native admin、MCP、Skill 与 agent resume 路径中重复拼接 permission key。
这让同一 operation 的 capability identity 可能在确认、恢复和执行前校验之间漂移。

## 决定

1. `CapabilityScope` 是 capability identity 的唯一 typed 表示；层级父 scope 的
   deny-first 匹配由该类型提供 candidates。
2. `AuthorizationRequest` 一次承载 session、canonical input、tool identity 和
   `OperationPolicy`；`AuthorizationEngine::authorize` 是唯一运行时决策入口。
3. `AuthorizationDecision` 和 `ConfirmationReceipt` 由 engine 产生，receipt 绑定
   typed capability、canonical input hash、policy revision、risk 和 expiry。
4. receipt 校验只接受原始 `AuthorizationRequest`；UI confirmation queue 保存同一
   request，恢复时不得重新从 renderer 参数猜测 policy。
5. agent、scheduled action、MCP、Skill、native Tauri admin 和外部打开入口都使用
   同一 request/decision/receipt 链路。provider/UI 的 `permission_key` 字符串只在
   config、event 和 catalog 边界投影，不参与 engine 内部匹配。

## 保留与删除

保留 deny-first、永久/会话 grant、Plan/AutoEdit/Autonomous、sandbox/network/path
边界和 TOCTOU receipt recheck。删除生产代码中的
`check_with_policy` / `verify_receipt_with_policy` 以及 receipt 的裸 permission-key
字段；不改变 config TOML、Tauri event 的外部字符串字段。

## 验证与回滚

正向、负向、父级 deny、receipt 输入/策略 revision、native queue 和全量工具安全矩阵
测试继续覆盖 `haven-tools`；执行 `cargo fmt --all -- --check`、`cargo check
--workspace --locked`、`cargo clippy --workspace --locked -- -D warnings` 与相关
workspace tests。回滚代码即可；config、数据库和 IPC event schema 无需重置。
