# ADR 0148：三层能力发现与按需工具详情

## 状态

已接受（2026-09-14）。本 ADR 细化 ADR 0145 的模型提示词目录交互；loader、session
隔离、权限矩阵和 provider `tools[]` schema 边界继续遵循 ADR 0145。

## 背景

Haven 的内置工具数量持续增加。把每个 operation 的名称、描述和 schema 都放进稳定提示词，
既增加上下文成本，也要求模型记住一张不适合当前任务的扁平清单。另一方面，模型在需要某个
能力时仍然需要一个可查询、可分页且不会意外执行工具的发现入口。

## 决定

模型可见的能力目录采用三层结构：

1. **第一层（提示词常驻）**：`system`、`agent`、`haven` 等 family 的整体用途、规避事项、
   root 名称和数量摘要。可选的 Skill/MCP 也只提供紧凑的名称或服务器摘要。
2. **第二层（`tool_catalog`）**：查询某个 family 下的 root，或描述一个 root（例如 `window`），
   返回 root 的整体说明和子 operation 名称/短摘要，不返回子 operation 的完整 schema。
3. **第三层（`tool_catalog`）**：用精确名称描述一个 operation（例如 `window.screenshot`），
   返回其描述、风险、指导、完整 `input_schema` 以及对应的加载提示。

`tool_catalog` 是 Safe、要求 session context 的只读控制面工具：

- `{"action":"list"}` 列出第一层 family；
- `{"action":"list","level":"tools"}` 列出第二层 root；
- 将 `level` 设为 `operations` 可列出完整第三层 operation，增加 `root` 可限定范围；
- `{"action":"describe","name":"window"}` 或 `list` 加 `root` 展开 root 的子项；
- `{"action":"describe","name":"window.screenshot"}` 查询第三层精确详情；
- `cursor`/`limit`/`next_cursor` 提供有界分页；发现动作不加载、不调用、不执行目标工具。

Builtin/Skill/MCP 的完整实现仍分别由 `DeferredToolCatalog`、Skill catalog 或 MCP cache
提供。模型获得第三层详情后，必须使用 `load_builtin`、`load_skill` 或 `load_mcp` 激活能力；
只有当前 session 已激活的能力才进入 provider `tools[]`，该结构化 surface 仍是实际调用 schema
的唯一权威来源。

## 未采用的方案

- **提示词平铺所有 operation**：实现简单，但上下文和记忆负担随安装能力增长。
- **只提供名称、不提供详情查询**：模型无法可靠获得精确 schema，也无法区分相近 operation。
- **查询时自动加载工具**：会改变 session provider surface，增加隐式副作用并破坏按需加载边界。
- **为每个 root 建一个发现工具**：会复制目录协议和安全处理；一个分页的 `tool_catalog` 足够覆盖
  builtin、Skill 与 MCP 三类来源。

## 影响

提示词更短且只承担方向导航；模型多一次或多次轻量 catalog 查询换取准确的 operation 选择。
`tool_catalog` 不改变数据库 schema、实体 ID、权限 key 或工具执行权限。MCP 未连接时只能列出
已配置服务器摘要；连接并发现后的缓存工具才能提供 operation 级详情，激活仍须通过 `load_mcp`。

实现边界说明：`crates/tools/src/builtin/tool_catalog.rs` 当前约 880 行，暂时将三层查询编排、
来源合并和响应裁剪放在同一私有模块，确保目录协议只有一个实现。若后续继续加入新的外部来源
并超过约 1000 行，应按来源适配器和响应渲染职责拆分，保持本 ADR 定义的 wire shape 不变。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --locked -p haven-tools`
- `cargo test --locked -p haven-agent`
- `cargo clippy --workspace --locked -- -D warnings`

## 回滚与重置

回滚代码即可恢复旧的提示词目录；本 ADR 不引入数据库或持久化格式变更，无需数据迁移或重置。
若回滚期间仍有会话在使用新的 `tool_catalog` 调用，结束该会话或按发布版本策略重置其运行状态，
避免旧版本把新的控制面工具视为未知能力。
