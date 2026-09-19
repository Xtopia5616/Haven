# ADR 0142：工具 Manifest、OperationSpec 与统一结果元数据

## 状态

Accepted — 2026-09-13

## 背景

工具分类已经统一，但工具身份、风险授权、UI 展示和 Prompt 目录仍然在后端、
前端及不同工具注册路径中重复维护。结果 renderer 也曾通过工具名或 payload
形状推断，容易让展示策略与实际执行策略漂移。

## 决定

1. `haven-common` 定义后端拥有的 `ToolManifest`，集中承载 `ToolIdentity`、模型
   Schema、`ToolPolicy`、展示、Prompt 和 availability。manifest 只通过目录/UI
   和权限相关边界传递，`ToolDef::json()` 及 provider `tools[]` 保持纯执行 Schema。
2. `haven-tools` 使用 `OperationSpec` 描述 operation view；运行时统一从
   `OperationPolicy` 读取风险、权限 key、确认、幂等性、scope 和并发策略。聚合
   工具仍保留独立的执行与信任边界，MCP、Skill 和 builtin 只共享注册/展示契约。
3. UI 删除 operation 元数据副本，启动时从 `get_tools` hydrate manifest；旧消息
   没有 manifest 时才按旧工具名规则回退。工具事件携带后端选择的 renderer，旧
   payload 形状推断仅用于兼容历史数据。
4. 工具可用性显式区分 `enabled`、`available`、缺少依赖原因、连接要求和权限
   要求；禁用不再被解释为不可用。
5. 每个 ReAct tool-call batch 只解析一次 `ToolCatalogSnapshot`。准入、action
   step 投影和执行共享这份快照；action card 携带已解析的风险/静默元数据，
   transcript 投影不得为每个工具重新查询 live registry。

## 影响与验证

工具名称、权限 key、数据库及历史消息格式保持兼容。manifest 是新增的 UI/catalog
契约；结果 payload 仍可按 operation 保持专属形状，不强迫所有工具使用大一统
Schema。renderer 迁移对旧消息保留确定性回退。

验证命令：

```text
cargo fmt --all -- --check
cargo check --workspace --locked
cargo test --locked -p haven-tools --lib
cd ui && corepack pnpm run check
cd ui && corepack pnpm run test:run
```

回退代码与本 ADR 即可，无需数据重置；provider schema 和现有工具名无需迁移。
