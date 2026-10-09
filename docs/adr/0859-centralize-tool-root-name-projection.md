# ADR 0859：集中工具 root name 投影

## 状态

Accepted — 2026-10-10

## 背景

Agent prompt builder 与 Tools catalog 各自定义了相同的 `tool_root(&ToolDef)`：优先返回 manifest `identity.root`，没有 manifest 时取工具 `name` 的第一段（`.` 前）。两处分别用于 prompt 的 root 摘要和 catalog detail 的 `root` 字段；根名却来自同一 canonical `ToolDef`，复制投影会让模型提示与目录响应对同一工具给出不同 root。

## 决定

- 在 Common 的 canonical `ToolDef` 上增加 `root_name()`，唯一拥有 manifest-root/名称首段的 fallback 规则。
- Agent prompt 与 Tools catalog 都调用 `ToolDef::root_name()`，删除两份私有 `tool_root` helper。
- 返回借用的 `&str`，避免每个 consumer 克隆根名；两侧继续自行处理截断、分组、计数和 wire shape。
- 不改变 manifest 优先级、名称分段、prompt 文案或 catalog JSON。

## 替代方案

- 继续保留两份同样的 helper：拒绝。同一 ToolDef 投影会随调用方变更而漂移。
- 把整段 prompt 或 catalog 编排移入 Common：拒绝。Common 只定义工具 root 的 canonical 派生规则；消费者的展示策略和输出结构各有 owner。
- 在 Agent 与 Tools 之间新建共享 crate/adapter：拒绝。`ToolDef` 已是 Common canonical contract，无需增加依赖或组合层。

## 影响与验证

这是 Common 类型 API 的纯借用 accessor 与 Agent/Tools 内部调用点调整；不改变 provider-facing ToolDef JSON、Tauri IPC、配置或持久化，无需数据重置。既有 prompt/catalog 行为断言保持原样；本轮按执行约束未运行测试套件。

验证通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、
`cargo clippy --workspace --locked -- -D warnings`、ADR index、ADR Prettier 与
`git diff --check`。未运行测试套件。

## 回滚

若 manifest root 与 Agent/Tools 的工具名 root 不再是同一 canonical 投影，应新增有明确领域语义的独立字段；否则整体撤回 accessor 与两个调用点并恢复原 helper。不新增旧函数别名。无持久数据需要重置。
