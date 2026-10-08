# ADR 0758：System power renderer 复用闭合状态值

## 状态

已采纳并实施。

## 背景

Windows power producer 将 `SYSTEM_POWER_STATUS` 明确映射到三种 `ac_power` 值（`offline` / `online` / `unknown`）和五种 `battery_status` 值（`high` / `low` / `critical` / `charging` / `unknown`）。`ToolSystemResult` props 与 system renderer guard 却把这两个展示字段都声明为任意字符串，使未识别值继续进入专用 power view。

## 决定

- UI `toolResultPresentation.ts` 定义 `ToolAcPowerState` 与 `ToolBatteryState` tuples/types/guards；System power props 和 nested validator 共用该值源。
- 当前之外的值回退 `ToolJsonResult`。其它 OS/network display strings 仍按开放文本处理，不套用这些 Windows power 值域。

## 替代方案

保留任意字符串会弱化固定 producer 映射与 UI 状态标签的一致性；扩展为通用系统状态 enum 会错误合并独立且开放的 OS/network 描述。

## 影响与验证

只收紧 UI System power renderer props 与 guard，不改 Windows API adapter、ToolResult JSON、IPC 或持久化。测试覆盖未知 `ac_power` 与 `battery_status` 回退，同时现有合法 `offline` / `unknown` 电源渲染用例继续通过；UI check 与 test:run 通过。

## 回滚

恢复开放字符串 props 与 guard 即可。没有 IPC、数据或持久化迁移。
