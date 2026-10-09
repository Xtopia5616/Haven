# ADR 0838：补齐 UI 命令 owner 门禁清单

## 状态

Accepted — 2026-10-09

## 背景

`check-ipc-contracts.ps1` 的 `ownedCommands` 登记了部分 UI command adapter 的旁路检查，但清单漏掉 `memoryCommands.ts::clear_facts`、`toolRunCommands.ts::list_tool_run_history` 和 `clear_tool_run_history`。这些命令已有唯一 adapter，却没有被 owner guard 覆盖；`MemoryView` 的显式直调断言也漏掉 `clear_facts`。架构文档只列 ToolRun board 的活跃列表与取消命令，没有列历史/清理操作和 Memory command owner。

## 决定

- 将 `clear_facts` 与 ToolRun history list/clear 加入 `ownedCommands`，使任何其它 UI 文件对这些 literal command 的直接 invoke 都失败。
- 更新 MemoryView 断言，明确禁止直接调用 `clear_facts`。
- 架构文档按实际 wrappers 列全 ToolRun 与 Memory 命令 owner，并把 Session lifecycle 的唯一 invoke owner 更新为 `sessionCommands.ts`。
- 不添加兼容路径；旧表中的不存在 Session history command 名已在 ADR 0837 清理。

## 替代方案

- 只依赖当前实现没有重复调用：拒绝。owner map 是防止未来旁路的静态门禁，当前实现唯一不代表清单完整。
- 为每个 command 创建单独 script 断言：拒绝。领域 owner map 已按源文件聚合，补齐实际清单即可。

## 影响与验证

只改 IPC 静态 owner 检查与架构文档，不改 Rust handler、UI wrapper、wire contract 或运行行为。验证：`scripts/check-ipc-contracts.ps1`、ADR 索引与 `git diff --check`。

## 回滚

恢复遗漏项之前的 owner 列表和文档段落即可；无需数据或配置重置。
