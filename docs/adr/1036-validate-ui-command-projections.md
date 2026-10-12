# 1036：校验 UI command 投影并保持单一命令边界

## 状态

已采纳并实施（2026-10-12）。

## 背景

UI command metadata registry 过去同时承载 invoke 包装职责；`open_external` 的副作用 orchestration 与 raw invoke 混在外链策略模块中。另有两个响应边界不足：bootstrap command 将未知 status 当成普通 loading，builtin manifest 重复 `stable_name` 会让列表与最后写入 Map 返回不同投影。

## 决定

- `contracts/commands.ts` 只保留 command boundary/security metadata；请求和响应类型引用 Rust 生成 contract。外链 invoke 由 `externalCommands.ts` 包装，`externalRef.ts` 只保留目标提取、点击分流与通知/错误编排。
- `get_bootstrap_status` response 使用生成的 `BootstrapStatus` 值集校验；未知值作为 probe failure 进入现有退避路径，合法 `loading` 保留快速轮询。
- builtin manifest 投影拒绝重复 `stable_name` 的整份新快照，清除有歧义的索引与列表。Skill/MCP 重复项沿用既有语义；manifest `input_schema` 仍是开放动态 JSON。

## 影响

Tauri request/response wire 不变。UI 对畸形 bootstrap 和 builtin manifest response 的处理变为失败关闭；外链目标和通知/错误行为保持原有流程。

## 验证

- UI command/contract focused tests、manifest 与 bootstrap tests
- `corepack pnpm run check`
- `scripts/check-ipc-contracts.ps1`、`scripts/check-ipc-events.ps1`
- `git diff --check`

## 回滚

回滚边界拆分时，仍须让 command 请求类型来自生成 contract；不得接受未知 bootstrap status，也不得把重复 builtin `stable_name` 暴露为自相矛盾的列表和 lookup map。
