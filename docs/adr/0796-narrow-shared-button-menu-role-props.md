# ADR 0796：收窄共享按钮与菜单项 role props

## 状态

已采纳并实施（2026-10-08）。

## 背景

全仓 UI Props 复核发现，`MaterialButton.role` 由主题外观页的 radio group 使用，所有调用值都是 `radio`；原 prop 却允许任意字符串。`MenuItem.role` 由模型与会话菜单调用，所有调用值仅为 `menuitem` 和 `menuitemradio`，没有动态 role 来源。

## 决定

1. 将 `MaterialButton.role` 限定为 `radio`，保留主题 radio group 的语义。
2. 将 `MenuItem.role` 限定为 `menuitem | menuitemradio`，保留 `menuitem` 默认值和现有调用。
3. 移除 `MenuItem` 单测中的 `as any`，让组件测试调用受同一 Props 契约检查。

## 影响与回滚

当前页面和可访问性语义不变：主题选项继续作为 radio，菜单项继续显式使用其两种实际角色。无 IPC、持久化、配置或数据迁移。若将来需要其他 ARIA role，应由新增的真实调用需求扩展对应组件合同；回滚时恢复开放 role prop。

## 验收

运行 UI 类型检查和完整 UI 测试；无外部 contract generator 影响。
