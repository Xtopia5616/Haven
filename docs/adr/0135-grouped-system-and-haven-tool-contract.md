# ADR 0135：system 与 haven 聚合工具的子操作安全契约

- 状态：已接受
- 日期：2026-09-12
- 范围：`haven-tools`、`haven-common`、UI、配置重置与安全文档

## 背景

`media` 与 `files` 已经是成熟的聚合工具；问题不在于“一个工具只能做一件事”，而在于
其它相关能力仍以多个平行根工具暴露，增加模型目录、权限键、配置开关和 UI renderer 的
维护面。与此同时，聚合工具不能用一个总风险覆盖所有子操作，否则会让低风险读取变得
过度受限，或让高风险写入失去明确的确认边界。

## 决定

1. `system` 保留机器信息、环境变量、注册表、电源和显示能力，并聚合
   `process`、`clipboard`、`input`、`window` 为 `scope` 子入口。
2. `haven` 聚合模型可见的 Haven 管理能力，以及 `actions`、`schedule`、`preferences`、
   `checklist`。管理 operation 使用原 operation 名；可能冲突的会话工具使用
   `actions_*`、`schedule_*`、`preferences_*`、`checklist_*` 前缀。
3. 聚合器只做 schema 分支组合、operation 路由和结果标注；执行仍委托给原子工具对象。
   每个分支分别委托 `risk_level`、`idempotency`、`operation_scope` 和 `concurrency`，
   因而同一根工具可以同时包含 Safe/Low/Medium/High/Critical 操作。
4. `media`、`files`、`shell`、`http`、`notify`、`agent`、`memory`、`ask`、`load_skill`
   和 `load_mcp` 保持独立根入口：它们各自拥有媒体/provider、文件路径、网络、通知、
   协作、记忆或动态扩展的独立生命周期与安全边界。
5. 删除旧模型根名 `process`、`clipboard`、`input`、`window`、`actions`、`schedule`、
   `preferences`、`checklist` 以及 `haven_diagnostics`、`haven_config`、`haven_skills`、
   `haven_tools`、`haven_mcp`。旧 tool settings 和权限键触发配置备份/重置，不做别名迁移。
   `SelfTool` 继续只用于 native Tauri 命令。

## 替代方案

- 把所有能力塞进 `haven`：拒绝，system 的机器/桌面边界与 Haven 管理边界不同。
- 继续保留所有平行根工具：拒绝，目录和权限维护重复，模型需要在多个入口间猜测。
- 为每个子操作重新复制实现：拒绝，会产生双重 schema、风险和执行真相。
- 以聚合根的最高风险作为所有调用风险：拒绝，会破坏低风险读路径，也掩盖真正的高风险
  operation；改为按 schema 路由到子工具的逐操作策略。

## 影响

模型工具目录和权限键发生破坏性变化。`system:process:kill`、`system:power:hibernate`
和 `haven:mcp_add` 等高风险操作继续单独确认；`system:info`、`haven:mcp_list` 等读取
仍按各自低风险策略执行。UI 根据 system 的 `scope` 和 haven 的 operation 前缀复用现有
专用结果卡片。配置、未完成快照和历史调用不保证旧根名兼容。

## 验证

- 聚合 schema 的正/负分支校验。
- system/haven 子操作风险、权限键和安全矩阵覆盖测试。
- `cargo test --locked -p haven-common`、`cargo test --locked -p haven-tools`、
  workspace check/clippy/test 及 UI check/test/build。

## 回滚 / 重置

回退本 ADR 对应提交即可恢复旧根入口；新旧版本之间不得混用配置、权限或未完成快照。
升级时若检测到旧根名，配置加载器备份并使用默认配置，按
`docs/release-and-reset.md` 完整重置。
