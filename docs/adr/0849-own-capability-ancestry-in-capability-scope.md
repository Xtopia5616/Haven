# ADR 0849：由 CapabilityScope 独占 capability ancestry

## 状态

已完成（2026-10-10）。

## 背景

ADR 0163 已规定 `CapabilityScope` 是 capability identity 的唯一 typed 表示，授权代码通过其父级 candidates 执行层级匹配。当前 `CapabilityScope::candidates()` 却只是把值转给 Common 顶层公开函数 `permission_key_candidates()`，再将字符串逐一包装回 `CapabilityScope`。全仓只有这一处生产调用；独立测试还对 `system.power.lock` 重复做了完全相同的断言。

这使唯一 typed owner 之外暴露了一个无独立消费者的字符串 API，也让 ancestry 规则的实现不在其领域类型旁边。

## 决定

1. 将点号父级遍历内联到 `CapabilityScope::candidates()`，由 typed capability identity 直接返回自身与父级 scopes。
2. 删除 Common 顶层 `permission_key_candidates()`，不保留兼容 alias；将单层 scope 行为补充到 `CapabilityScope` 测试中，删除重复的自由函数断言。
3. 保持 ancestry 顺序、匹配边界及无点号 scope 的结果不变；冒号格式仍由现有 capability 校验拒绝。

## 替代方案

- 保留自由函数并降低可见性：拒绝。它没有独立消费者或复用价值，额外 helper 仍会把单一 owner 的规则拆成两段。
- 在授权调用方分别计算 ancestry：拒绝。会复制匹配规则并违反 `CapabilityScope` 的唯一 typed identity 决策。

## 影响与验证

仅删除仓库内无独立调用者的 Common 公共函数并收敛实现 owner，不改变 authorization key、持久配置、IPC、匹配顺序或安全行为。无数据重置要求。验证：`cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、ADR 索引/链接检查、旧 helper 全仓搜索与 `git diff --check`。

## 回滚

将 ancestry 循环移回顶层 helper，并恢复 `CapabilityScope::candidates()` 的委托调用与原测试即可；无数据或运行态回滚步骤。
