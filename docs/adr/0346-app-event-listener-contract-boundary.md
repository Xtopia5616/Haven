# ADR 0346：App event listener contract boundary

- 状态：已采纳（2026-09-25）
- 范围：app-shell Tauri event listeners、`contracts/app.ts` mapper、布局与 ToolsView 的 app event effects
- 关联：[ADR 0330](0330-session-lifecycle-ui-contract-mapper.md)、[ADR 0335](0335-action-board-ui-contract-mapper.md)、[ADR 0340](0340-recording-event-contract-audit.md)、[ADR 0341](0341-settings-command-contract-boundary.md)

## 背景与审计

`crates/app-binary/src/events.rs` 持有 app-shell channel 名称和 wire DTO；Agent 变体由
`event_bridge.rs::TauriEmitter` 映射并发出，bootstrap、MCP/Skills、hotkey、mute/tray 与 LLM
配置事件由 app 命令或启动 handler 发出。前端 `contracts/app.ts::mapAppEvent` 是现存唯一
app-shell snake_case 字段映射点。

审计发现 ToolsView 对 `mcp:status_change` 和 `skills:status_change` 使用通用 `registerOne`，虽然不读取
payload，却绕过了 app contract mapper；布局也为 `skills:status_change` 注册了无副作用的空 handler。
MCP 的布局通知和 ToolsView refresh 是两个不同 UI 副作用，不能合并或去重。`notification:show` 是
Agent contract 的事件，由 `mapAgentEvent` 和布局通知 handler 处理，不属于 app-shell contract。

app contract 中没有第二个 app wire interface 或重复字段转换。现有 `AppWirePayloadMap` 是手写的 Rust
wire shape 镜像；消费 DTO 与 wire DTO 分开定义是 mapper 的输入/输出边界。Rust MCP status 使用 serde
外部标记 enum，passthrough 分支不校验其 variant，因此会保留未知状态变体和附加字段；显式投影的
hotkey/interaction 事件只输出已知消费字段。Resume response 的
`interactions` 仍由 `sessionReducer/interaction.ts` 做容错归一化；它还接受 camelCase 输入且拥有 session
恢复行为，不在本次范围内提取或重写。

## 决定

1. 在 `events.ts` 用唯一的 `adaptAppEvent` 调用 `mapAppEvent`。批量 `appEventListeners` 和新增的单条
   `registerAppListener` 都复用它；route/store handler 只接收映射后的 app DTO。
2. ToolsView 的 MCP 与 Skills refresh listener 改用 `registerAppListener`。布局移除无副作用的 Skills
   listener；布局继续拥有 MCP 通知，ToolsView 继续拥有 MCP 列表刷新和 Skills 列表刷新。
3. 保持 Rust DTO、channel、payload、Tauri producer、注册顺序、到达顺序和通知行为不变。MCP 未知 status
   variant 与 pass-through 附加字段继续原样保留；hotkey 显式映射继续忽略未知扩展字段。
4. 不增加 app payload validator 或 enum/error fallback。当前 mapper/consumer 对 malformed payload 的既有
   行为不变；新增入口仍用 `protectEventCallback` 隔离同步错误和异步 handler rejection。
5. 不改 session/action/recording/settings contract、Rust 事件协议或全局 codegen。

## 替代方案

- 让 ToolsView 继续使用 `registerOne`：会让 app event listener 有 mapper 与 raw payload 两种入口，拒绝。
- 把 MCP refresh 移到布局或把两类副作用合成一次 handler：会改变页面生命周期和 notification/refresh
  owner，拒绝。
- 删除 MCP event 的未知变体/附加字段或增加封闭 enum validator：会改变当前 pass-through 兼容行为，拒绝。
- 把 resume interaction normalizer 合并进 app event mapper：会触及 session restore 的 camelCase 兼容与
  malformed fallback，超出本次 app event 切片范围。

## 影响与验证

该切片只改 TypeScript event adapter、ToolsView listener wiring、空订阅清理、回归测试和文档。新增测试确认
批量与单条 listener 都经同一 mapper、hotkey 映射忽略 wire 扩展字段、MCP 的未来 enum/扩展字段透传。
MCP layout toast、ToolsView MCP refresh、ToolsView Skills refresh 与 `notification:show` 的通知顺序和副作用
保持不变。无持久化、配置或数据迁移。

验收命令：

```sh
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
corepack pnpm --dir ui run build
pwsh -NoProfile -File scripts/check-ipc-events.ps1
pwsh -NoProfile -File scripts/check-ipc-contracts.ps1
git diff --check
```

## 回滚

回滚本提交可恢复 ToolsView 使用 `registerOne`，并恢复布局中的空 Skills listener；无需 Rust 修改、IPC
变更、配置迁移或数据重置。
