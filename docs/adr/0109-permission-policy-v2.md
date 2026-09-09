# ADR 0109：统一权限策略与安全确认边界

## 背景

旧权限系统把“什么时候询问”拆成 `confirmation_mode` 与
`min_risk_level` 两个独立配置，同时把永久规则、会话信任、工具禁用、路径沙箱
和确认弹窗分散在多个调用点。这样既增加了配置理解成本，也让确认事件容易把
原始工具参数（命令、URL、文件内容或扩展参数）带入 renderer。

## 决定

1. 由 `haven-tools::AuthorizationEngine` 作为所有 builtin、MCP、skill、UI 直调和
   定时触发路径的唯一授权核心。
2. 用 `SecurityConfig.permission_mode` 替代旧的双字段策略：
   `balanced`（Medium+）、`careful`（Low+）、`manual`（全部）、`autonomous`（仅
   Critical）。策略变更会清除会话信任；永久规则仍保留 deny-first 和权限键父级继承。
3. 规则管理继续使用 `tool` / `tool:operation` 精确键。允许写精确键，拒绝写工具根键，
   避免“始终拒绝”只覆盖当前一次操作的误解。
4. `confirm:requested` 只发送后端生成的 `summary`，不再发送原始 `params`。原始参数
   只保留在后端待确认状态中，确认结果仍由后端按 step id、效果和范围重新校验。
5. 设置页提供“权限中心”，显示可读规则标签、精确键和效果，并提供撤销单条规则与
   清除全部规则；清除规则不改变用户选中的默认策略。
6. 旧配置不做隐式字段迁移。检测到 `confirmation_mode` 或 `min_risk_level` 时备份
   配置并使用平衡默认值，用户需要重新建立权限策略。

## 替代方案

- 保留旧的模式 + 阈值组合：拒绝，用户需要理解无效或冲突组合，且策略边界不清楚。
- 把完整参数继续发送到 UI，再由 UI 做脱敏：拒绝，renderer 不是安全边界，且每种
  新扩展工具都可能遗漏脱敏规则。
- 只在确认弹窗上做权限判断：拒绝，MCP、skill、定时和 native UI 入口会产生绕过路径。

## 影响

- `config.toml` 的 `[security]` 段发生破坏性变化，必须重建旧权限策略；数据库 schema
  不变。
- 前端确认交互不再显示原始命令/路径/请求参数，改为后端摘要，降低秘密泄漏和误操作风险。
- `reset_permissions` 会清除永久规则与会话信任，但保留当前默认策略。

## 验证

```text
cargo fmt --check
cargo test --workspace --locked
cargo clippy --workspace --locked -- -D warnings
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
corepack pnpm --dir ui run build
```

重点回归：四种策略的边界、策略变更清除会话授权、父子权限 deny-first、确认摘要不含
原始敏感参数、旧配置备份，以及 reset_permissions 的持久化原子性。

## 回滚与重置

回滚代码即可恢复旧二进制行为，但新版本写出的 `permission_mode` 配置不能交给旧二进制
解释。回滚前恢复升级前的完整数据根目录，或删除 `config.toml` 后重新配置；不混用新旧
配置文件。数据库无需因本 ADR 单独重置。
