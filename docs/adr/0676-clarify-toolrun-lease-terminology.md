# ADR 0676：澄清 ToolRun 持久化与 lease 术语

## 背景

`ToolRunLease` 的模块注释称其由 tools runtime 和 “action persistence” 共用；`ToolsFacade::set_admin_context` 的注释称独立注入 “durable action storage”。两处指的都是通过 `StartupWiring::tool_run_store` 注入的 `ToolRunStore`，旧泛词使已统一为 ToolRun 的持久工作单元又出现 action 这个别名。

Lease 内的 `claim_token` 标识被领取的 ToolRun，或 ToolRun completion result；它不是领取 consumer 的身份。

## 决定

- 两处持久化说明统一称为 Tools runtime 与 ToolRun persistence/storage。
- `claim_token` 注释改为说明其标识 ToolRun 或 completion result，而非 consumer。
- 在命名规范中记录 lease identity 的角色；不更改公共符号、数据库字段、持久化内容或行为。

## 考虑过的方案

- 保留 action 一词：它没有在该处指向独立的 action 实体或 API，只是 ToolRun persistence 的历史泛称。
- 将 `claim_token` 改名：当前名称已准确表达 lease 内的 claim key，补足含义注释即可，不需要扩大改动面。

## 验证

- 全仓 `action persistence` / `durable action storage` 搜索确认引用已清除。
- ADR 索引检查与 `git diff --check`
- 未运行测试；本轮仅更新注释与架构词汇。

## 回滚与重置

恢复旧注释与文档条目即可回滚。没有配置、持久化或 wire shape 变化，无需重置。
