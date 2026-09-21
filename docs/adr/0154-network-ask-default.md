# ADR 0154：网络策略默认改为请求确认

- 状态：accepted
- 日期：2026-09-21
- 范围：`haven_common`、`haven_tools`、`haven_mcp`、`haven_app-binary`、Settings UI
- supersedes：ADR 0152 中“默认 `restricted`”的默认值决定；不改变 `deny`、`restricted`、`open` 的既有显式语义

## 背景

网络策略同时承担了技术边界和用户体验信号。原默认值 `restricted` 对普通 HTTP 请求本身可以继续做 SSRF/DNS 校验，但对无法由 Haven 审计目的地的 MCP、技能和子进程能力会直接拒绝。用户很难区分“需要确认”和“技术上不可约束”，导致默认配置表现为网络不可用。

## 决定

1. 增加 `NetworkPolicy::Ask`，持久化值为 `ask`，并作为 `SecurityConfig`、授权引擎和 MCP manager 的默认值。
2. `ask` 对可验证的公网目的地沿用受限网络的 DNS/SSRF/重定向防护；普通网络操作仍由 `AuthorizationEngine` 进入确认流程。
3. `deny` 继续阻断所有网络能力；`restricted` 保持原有语义；`open` 仍是显式关闭全局目的地限制的高级选项。
4. `ask` 不放开 opaque 子进程。`workspace_write` 下仍拒绝无法约束的 MCP/技能/脚本；只有现有的 `full_access + open` 组合可以启动这类能力。

## 影响与验证

缺少 `network_policy` 的新配置默认进入 `ask`；已有显式 `restricted`/`deny`/`open` 配置不变，无需数据迁移。新增枚举会让旧二进制无法理解新写入的 `ask` 配置，回滚前应恢复升级前的配置备份或手工删除该字段。

验证覆盖默认配置、HTTP 请求确认、opaque 子进程负向边界、MCP HTTP 的公共地址校验，以及 Settings UI 的可选策略。
