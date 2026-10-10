# ADR 0822：默认关闭 MCP launcher 的 GUI 自动启动

## 状态

Accepted — 2026-10-09

## 背景

MCP stdio server 是用户配置的外部进程。部分 launcher 在 MCP 客户端连接时还会启动 IDA 等 GUI 程序；单纯执行 `mcp_reload` 因而可能带来可见窗口和额外进程。Haven 拥有 MCP 子进程创建边界，但不拥有任意外部 launcher 的内部行为。

## 决定

Haven 启动每个 MCP stdio server 时，默认向子进程传入 `MCP_AUTOLAUNCH=0`。该 server 的显式环境配置在此默认值之后应用，可以为确实需要 GUI 自动启动的 launcher 设置 `MCP_AUTOLAUNCH=1`。遵守此约定的 launcher 必须把缺省和 `0` 解释为不启动 GUI。

这只是传给 MCP 子进程的 opt-out 默认值；Haven 无法阻止忽略该变量的任意第三方进程启动 GUI。具体 launcher 仍须实现并检查该环境变量。

`haven.mcp.mcp_reload` 的内建风险等级提升为 High，使调用前的授权与风险展示反映它可能启动配置的外部进程。

## 替代方案

- 禁止 `mcp_reload` 启动所有 stdio server：拒绝。这会使正常的 stdio MCP 连接无法建立，破坏 MCP 配置的既有用途。
- 由 Haven 推断外部进程是否为 GUI 并拦截启动：拒绝。MCP server 可以在连接后自行创建子进程，Haven 没有稳定、跨 server 的 GUI 识别契约。

## 影响与验证

MCP 协议、配置文件格式、IPC 和持久数据不变。所有 stdio server 会看到默认环境变量；支持显式环境设置的 launcher 可按 server 覆盖。`mcp_reload` 风险从 Medium 提升为 High。尚未在本仓库找到 IDA launcher 源码，因此本 ADR 不声称已验证该 launcher 是否遵守此约定。

适用门禁：`cargo fmt --all -- --check`、`cargo test --locked -p haven-mcp`、`cargo clippy --locked -p haven-mcp -- -D warnings`。

## 回滚

移除 MCP 子进程的默认环境变量注入即可恢复原有继承环境行为；无需数据库或配置重置。
