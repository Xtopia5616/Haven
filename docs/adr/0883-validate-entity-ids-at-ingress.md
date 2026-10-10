# ADR 0883：在输入边界统一校验 Haven 实体 ID

## 状态

Accepted — 2026-10-10

## 背景

全项目命名审查发现多数 Tauri 命令直接把 renderer 提供的会话、确认、ToolRun、Fact 和消息 ID 交给 actor、store 或副作用路径；`process_transcript` 还会在 Agent 校验活动会话前先持久化附件。配置凭据引用虽然已有格式校验，但在 `haven_common::config::credentials` 内重复实现了 UUID32 检查逻辑，与 Common 的规范不一致。

另外，消息 inbox 的 `claim_token` 已通过 `new_id("claim")` 生成，并只保存在 Tools 进程内领取表，但 ID 前缀表未登记该令牌。

## 决定

- App 命令使用共享 `validate_command_id` 包装器调用 `haven_common::types::is_canonical_id`，在访问 store、actor 或执行副作用前验证对应前缀。当前覆盖 Session、确认请求及其 owner、回滚目标用户消息、录音入口、ToolRun 查询/删除/取消、Fact 删除和 session 权限撤销。
- 凭据引用校验保留领域错误语义，但格式判断直接调用 Common 的 `is_canonical_id(reference, "cred")`。
- 将进程内消息领取令牌 `claim-*` 登记为独立的 `haven_tools` ID 空间；它不落库，确认后即从内存领取表移除。
- 外部 provider/model 标识、MCP session、配置 profile ID、工具调用代次和其他非 Haven 实体标识仍遵循各自协议，不套用实体 ID 前缀。

## 影响与兼容性

不改变有效 ID 的 wire shape、数据库数据或配置结构；格式错误、前缀错误和大小写错误的 renderer 参数现在会在命令边界被拒绝。无需数据重置，不保留旧格式兼容入口。`claim_token` 仅作为一次性进程内工具结果在使用，不形成持久化或恢复契约。

## 验证

通过：`cargo fmt --all -- --check`、`cargo test --workspace --locked --quiet`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`pwsh -NoProfile -File scripts/check-ipc-contracts.ps1`、`corepack pnpm run check`、`corepack pnpm run test:run`、`corepack pnpm run build`、ADR Prettier 检查与 `git diff --check`。其中 Rust 全测及 UI 全测通过，命令数为 79。该 ADR 不代表 UI runtime mapper、所有内部 store 或所有持久化写入路径的实体 ID 审计已经完成；剩余范围继续由路线图 §5.7 跟踪。

## 回滚

若合法生成的 Haven ID 被拒绝，应先修正其 owner 的生成或前缀定义；不得恢复 crate-local 格式解析或放宽为任意非空字符串。
