# ADR 0797：共享 MaterialSelect 的 value/label option 类型

## 状态

已采纳并实施（2026-10-08）。

## 背景

MaterialSelect 自有 `SelectOption` shape；ModelConfigCard、ProviderList 与 MediaSettings 又重复声明或内联相同的 `{ value: string; label: string }`。这些数组最终都进入共享 MaterialSelect，分散声明会让组件 props、回调和生产数据的字段约束漂移。

## 决定

将选项行定义为 `ui/src/lib/selectOption.ts::SelectOption`，由 MaterialSelect、ModelConfigCard、ProviderList 和 MediaSettings 共用。`group` 仍是可选字段，允许共享选择器分组；各调用方的 option 值和标签生成规则继续由其所属页面负责。

## 影响与回滚

仅统一 UI 内部 value/label 行类型，不改变选项顺序、筛选行为、组件交互或外部契约；无 IPC、持久化或配置迁移。回滚时可恢复组件局部结构声明，但会重新引入类型漂移风险。

## 验收

运行 UI 类型检查和完整 UI 测试；无 Rust 或 IPC generator 影响。
