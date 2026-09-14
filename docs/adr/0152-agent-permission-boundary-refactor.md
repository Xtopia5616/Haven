# ADR 0152：Agent 权限边界重构

- 状态：accepted
- 日期：2026-09-14
- 范围：`haven_common`、`haven_tools`、`haven_agent`、`haven_app-binary`、Settings UI

## 背景

旧模型用一个 `PermissionMode` 同时表达“什么时候询问”和“技术上能访问什么”。这会把只读、编辑、命令执行、网络访问和永久授权混成一条风险阈值，导致三个问题：只读操作无法被机器识别；UI 的全量设置保存可能覆盖权限规则；确认请求携带原始参数，容易让 renderer/日志边界变得脆弱。

本次允许破坏性重构，目标是把权限拆成可审计的策略层，同时保持所有工具调用经过同一个后端授权入口。

## 调研结论

| Agent | 关键做法 | 对 Haven 的启示 |
|---|---|---|
| Codex | 沙箱定义写入目录、受保护路径和网络边界；approval policy 决定何时询问，两者分开；工具活动、批准和网络策略会被记录。 | “能做什么”和“是否现在询问”必须是两个轴。 |
| Claude Code | allow/deny 规则、作用域和模式分层；deny 优先；`plan`、编辑自动接受和绕过确认是不同模式；管理策略可覆盖用户策略。 | 规则要有明确优先级、作用域和全局禁用底线。 |
| Gemini CLI | policy engine 以工具/服务器、参数模式、决策（allow/deny/ask）和优先级组成；沙箱扩容是一次性、具体范围的批准。 | 权限身份要细到 operation/MCP server/tool，并绑定精确请求。 |
| OpenHands | 无隔离的进程模式几乎等同主机权限；Docker 模式才提供容器边界，但挂载目录仍可被修改。 | “full access”必须是显式逃生舱，不能被文案误认为已经有 OS 隔离。 |

主要资料： [OpenAI Codex 安全说明](https://openai.com/index/running-codex-safely/)、[Codex CLI 权限说明](https://learn.chatgpt.com/docs/codex/cli)、[Claude Code IAM](https://code.claude.com/docs/en/iam)、[Gemini policy engine](https://github.com/google-gemini/gemini-cli/blob/main/docs/reference/policy-engine.md)、[Gemini sandbox](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/sandbox.md)、[OpenHands process sandbox](https://docs.openhands.dev/openhands/usage/sandboxes/process)。

## 决策

### 1. 互相独立的策略轴

- `PermissionMode` 改为 `plan`、`default`、`auto_edit`、`autonomous`。它只定义确认体验。
- `OperationPolicy` 同时声明 effect、data sensitivity、network access、risk 和执行契约；`concurrency = ReadOnly` 不再被当作“安全读取”的同义词。
- `SandboxMode` 改为 `read_only`、`workspace_write`、`full_access`，并支持可选的绝对 `writable_roots`。`read_only` 在授权网关拦截修改，`workspace_write` 叠加路径规范化检查并拒绝无法约束的 opaque 子进程；Windows 子进程还加入 Job Object 的进程树/生命周期约束。Job Object 不是 AppContainer，不能伪装成完整的文件系统或网络隔离。
- `NetworkPolicy` 改为 `deny`、`restricted`、`open`。`restricted` 保留 HTTP/MCP 的 SSRF、私网/元数据地址拦截、DNS 地址固定和禁止跨源重定向；`deny` 阻断所有声明为 Public/Opaque 的网络能力，包括 HTTP、MCP、Skill、Shell 和会调用外部 provider 的操作。

默认值是 `default + workspace_write + restricted`，兼顾桌面助手可用性和现有安全默认。

### 2. 决策顺序与硬底线

授权顺序固定为：技术边界与禁用操作 → Plan 的能力阻断 → `Required`/`Critical` 强制确认底线 → deny 规则 → allow 规则 → 模式确认决策。父级 deny 覆盖子级 allow；永久/会话 allow 不能绕过 Plan、Critical 或 `Required`。

每一个需要确认的请求生成后端 `ConfirmationReceipt`，绑定：规范化输入 hash、权限 key、有效风险、策略 revision、过期时间。执行前必须再次验证；配置变化、风险升高、输入变化、路径/网络边界变化都会使收据失效。`ConfirmationResult` 不再跨层携带原始参数，参数只由已经持有请求的后端调用链继续传递。

### 3. 配置和 UI 契约

- `ToolConfig.risk_override` 改为 typed `RiskLevel`，拼写错误在配置加载时失败，不再静默忽略。
- 权限规则的增删和普通 Settings 保存分离；`update_settings` 明确保留当前规则，防止 stale form 清空永久授权。
- UI 同时展示确认模式、文件沙箱、网络策略和已保存规则；文案明确沙箱与确认是两道不同边界。
- 现有 `allowed_domains`、DNS 固定和同源重定向约束继续由 HTTP 工具执行，MCP HTTP 连接也执行同等约束，不能仅依赖模型提示或 renderer 选择。

## 不变量与验证

1. 所有 Agent、MCP、Skill、scheduled action 和 native UI 调用都先经过 `AuthorizationEngine`。
2. 前端没有 `confirmed` 绕过路径；确认只能凭 receipt 恢复。
3. plan 模式允许显式只读 operation，写操作直接阻断且不受 Always allow 影响；auto_edit 只自动放行明确的工作区文件编辑，UI、调度、命令、网络和删除仍询问。
4. effect、data sensitivity、network access 是独立契约；敏感读取和数据外传不能因为“不修改数据”而自动放行。
5. deny 优先、Critical 不可被授权绕过、网络策略和 sandbox 是后端硬拒绝；opaque 子进程只有在 FullAccess + Open 的显式组合下才可启动。
6. `cargo check --workspace --locked`、`cargo test --locked -p haven-tools`、`corepack pnpm run check` 必须通过。

## 兼容性和回滚

这是测试阶段允许的破坏性配置重构。旧 `permission_mode` 值不迁移；配置加载器按现有策略备份并以默认配置启动。需要回滚时恢复本 ADR 之前的提交，同时按 `docs/release-and-reset.md` 删除或重建 `[security]` 段；数据库 schema 不受本次变更影响。
