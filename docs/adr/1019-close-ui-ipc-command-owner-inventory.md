# 1019：闭合 UI IPC 命令所有者清单

## 状态

已采纳并实施（2026-10-12）。

## 背景

IPC checker 会阻止已登记命令被其它 UI 文件绕过 owner adapter，但不会发现 owner 清单自身漏项。复核发现 `toolsCommands.ts` 已拥有 `list_skills`，owner map 却未登记，因此未来新增绕过调用不会触发该检查。

## 决定

- 将 `list_skills` 加入 `toolsCommands.ts` 的 command owner 清单。
- checker 将生产 UI 中全部字面量 `invoke` 命令与 owner 清单做精确集合比较；新增未登记命令和陈旧登记都会失败。
- checker 拒绝同一命令被多个文件登记为 owner；测试文件不计入生产调用集合。

## 影响

只加强 IPC 所有权静态门禁，不改变命令、wire payload、运行时行为或安全策略，无需数据迁移或重置。

## 验证

- `scripts/check-ipc-contracts.ps1` 验证生成契约、注册、安全目录、文档和 UI 命令所有权。
- `scripts/check-adr-index.ps1` 验证 ADR 索引与链接。
- `git diff --check` 验证补丁格式。

## 回滚

回滚 owner map 精确集合检查会重新允许未登记的生产 UI invoke 命令逃过旁路检查。
