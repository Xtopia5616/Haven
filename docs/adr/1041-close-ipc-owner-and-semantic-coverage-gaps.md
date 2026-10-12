# 1041：关闭 IPC owner 与语义校验覆盖缺口

## 状态

已采纳并实施（2026-10-12）。

## 背景

ADR 1040 建立了全部 79 个命令响应与 36 个事件 channel 的 Rust 生成结构校验，但复核发现 renderer 命令 owner 清单只覆盖 73 个实际调用，尚未记录其余 6 个命令为何不接入 UI；调用扫描器也只识别单引号形式。事件 checker 验证 channel/DTO 与 mapper 覆盖，却没有约束 Rust/Tauri emit callsite。另有两个消费边界只检查结构或有限数值：Session permission grant 的 capability 字符串可能不符合 Rust `CapabilityScope` 规则，usage 成本可以是负数。

## 决定

- IPC checker 为所有生成命令要求唯一 disposition：UI owner 或带理由的 reviewed non-UI。production invoke 集必须与 UI owner 集完全相等；生成的 handler 集必须等于两种 disposition 的并集。
- invoke owner 检查识别单引号、双引号和模板字面量；生产代码中 Tauri core API 的直接导入只允许在 `tauri.ts`，测试 mock/import 只允许在 `tauri.test.ts`。
- event checker 维护 app-binary 直接 Tauri emit 文件及调用数清单。新增或移除 emit site 时必须更新清单并重新审查其 channel/DTO owner。
- Session permission parser 复用 Rust `CapabilityScope::try_new` 的语法：非空、无首尾空白、点分段非空，字符限 ASCII 字母/数字、下划线和连字符。
- Agent live usage 与 Session 恢复 usage 的 USD 成本必须有限且非负；未定价成本仍可为 `null`。
- 结构校验仍允许 DTO 增加未知字段。动态扩展载荷、未被消费的媒体估算字段和 provider 外部 ID 不在本次收紧范围。

## 影响

IPC 门禁现在覆盖完整命令 disposition，并能发现遗漏的 UI 调用 owner、Tauri core API 旁路和 app-binary emitter 改动。无效 capability 不再作为可撤销权限展示；负 usage 成本不再进入 renderer。Serde DTO、事件 payload、数据库与存储格式均未改变，无需数据库、配置或数据重置。

## 验证

- `pwsh -NoProfile -File scripts/check-ipc-contracts.ps1`
- `pwsh -NoProfile -File scripts/check-ipc-events.ps1`
- `corepack pnpm run check`
- `corepack pnpm run test:run`
- `corepack pnpm run build`

## 回滚

若合法 permission/cost 值被拒绝，先对照 Rust `CapabilityScope` 与 usage 计价 owner 修正 parser 和边界测试。若撤销 emitter inventory，必须同步恢复可审查的 Tauri emit 覆盖门禁；不能只删除计数检查。
