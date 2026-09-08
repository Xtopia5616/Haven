# ADR 0103：合并模型可见的诊断能力

日期：2026-09-08
状态：已采纳

## 背景

ADR 0070 将应用诊断拆成 `haven_diagnostics` 和
`haven_session_diagnostics` 两个模型工具。两者都是只读、低风险能力，且都由
`SelfTool` 的同一 structured surface 执行；前者的 `status` 还已经包含会话数量摘要。
两个工具在模型目录和 UI 中分开显示，造成能力重复和入口不统一。

## 决定

1. 删除 `haven_session_diagnostics` 模型工具，只保留 `haven_diagnostics`。
2. `haven_diagnostics` 的 operation allowlist 统一为 `status`、`logs_tail`、`sessions`、
   `errors`。
3. 保留各 operation 的原有边界：日志最多 500 行，会话查询最多 50 条；会话诊断仍只
   返回 id、状态、标题、时间和字符数，不返回 input/transcript 正文；日志继续逐行脱敏和截断。
4. 合并只影响模型可见 capability，不改变 `SelfTool` 的 native structured entry。日志
   operation 使用 `haven:diagnostics` 共享资源，会话 operation 使用
   `haven:sessions` 共享资源，避免无意义地串行化两类只读查询。
5. 新的会话权限 key 为 `haven_diagnostics:sessions` 和
   `haven_diagnostics:errors`。旧的 `haven_session_diagnostics` 权限名视为已删除工具名，
   配置加载时触发备份并要求重置；不静默把旧授权迁移到新的工具根键。由于权限 key 支持
   父级继承，已有的 `haven_diagnostics` 根授权也会覆盖该工具新增的会话 operation，作为
   本次合并接受的授权语义变化。

## 替代方案

- 只合并内部 service、继续暴露两个工具：能减少实现重复，但保留了模型目录中的视觉和
  选择重复，不符合统一诊断入口的目标。
- 保留两个工具并让 `status` 返回更多会话详情：仍有两个入口，而且会扩大单次健康检查
  的隐私输出，拒绝。

## 影响与验证

这是模型工具目录、权限 key 和 UI 工具身份的破坏性变更，不涉及数据库 schema、Tauri
命令或会话持久化格式。旧模型调用需要重新生成；包含旧诊断权限的配置会按发布与重置
策略生成备份并以默认配置启动。

重点验证：

```text
cargo fmt --all -- --check
cargo test --locked -p haven-common config::loader
cargo test --locked -p haven-tools builtin::admin
cargo test --locked -p haven-tools security
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
```

## 回滚与重置

回滚对应提交即可恢复两个 capability 的注册和旧 operation 权限矩阵。回滚或升级前若
配置已经因旧工具名触发备份，应恢复同一版本的完整数据根目录；不要把新旧版本的权限
配置混用。数据库不需要重置。
