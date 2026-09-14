# ADR 0145：分层能力目录与按 session 延迟加载

## 状态

已接受（2026-09-14）。本 ADR supersede ADR 0137 中“启用 Skill 直接注册、删除
`load_skill`”的模型目录部分；点号 operation view、权限 key 和 UI renderer 的正式名称
仍保持不变。

提示词目录的可见粒度由 [ADR 0148](0148-three-layer-capability-discovery.md) 进一步收窄：
本 ADR 的 loader、session 隔离与 provider schema 边界仍然有效。

## 背景

Builtin operation、Skill 和 MCP 的完整 JSON schema 都放进每次 provider 请求，会让系统
提示词和 `tools[]` 随安装能力线性膨胀。大多数请求只使用少数能力，却要为所有能力支付上下文
成本；同时把所有 executable adapter 放进全局模型注册表，也让 session 隔离和 MCP 的渐进加载
边界不够清晰。

## 决定

Haven 采用三层模型-facing capability surface：

1. **核心常驻层**：只保留轻量、路由性质或高频的工具，例如 `ask`、`notify`、加载器以及
   少量基础读取 operation。它们在 global `ToolRegistry` 中注册，每次请求都可以使用。
2. **延迟 builtin / Skill 层**：完整实现和 schema 保留在 host-owned `DeferredToolCatalog`，
   但不进入 provider `tools[]`。模型先从第一层 family/root 索引和 `tool_catalog` 查询看到
   operation，再调用 `load_builtin` 或 `load_skill`。loader 在当前 `SessionCatalog` 中以
   原子批次注册选中的工具，并受 `max_tools_per_request` 限制。
3. **MCP 层**：模型只看到服务器名、描述和工具数量/短名称索引；`load_mcp` 仍按服务器发现
   并把结果注册到当前 session。服务器加载结果只返回紧凑摘要，完整 MCP schema 在下一次请求
   的 `tools[]` 中出现。

所有层都使用同一份 `ToolDef`、manifest、权限矩阵和执行实现。provider-facing 定义只由
`list_defs_for_session` 生成：global core registry 加当前 session overlay；未加载的 deferred
名称不能被 provider 直接执行。控制面仍可通过 `ToolsManager::get_tool` 找到 deferred
实现，用于设置、诊断和重建，不改变模型可见边界。

## 交互与恢复契约

- `load_builtin` 接受精确 `operations` 或 `roots`，去重后在预算内一次性加载；超预算返回可供
  模型缩小选择的紧凑列表，不部分写入。
- `load_skill` 接受 Skill 名称（兼容 `skill__name` 和展示名），按 session 加载对应 adapter；
  它不会把其它 Skill schema 带入当前请求。
- loader 都要求 session context，属于 Safe control-plane operation；延迟工具仍必须覆盖安全
  矩阵，不能因为不在 global registry 就绕过授权契约。
- ReAct snapshot / resume 持久化并重放 loader 的选择参数；重复重放是幂等的。MCP 的既有按服务器
  恢复路径保持不变。
- prompt 只渲染紧凑索引，不重复嵌入全量 schema。工具 schema 仍由 provider 的结构化 `tools[]`
  传递，避免把 JSON schema 再复制到系统提示词。

## 未采用的方案

- **每轮发送全部 schema**：实现最简单，但上下文成本随安装能力线性增长，并不能利用 session
  实际使用范围。
- **为每个能力增加独立动态 provider/API**：会重复 ToolDef、权限和执行路径，制造第二份契约；
  本方案只增加两个通用 builtin loader，并复用现有 SessionCatalog。
- **只在 prompt 中写名称、不提供结构化 loader**：模型无法可靠获得目标 schema，也无法让预算、
  session 隔离和 resume 共享同一执行边界。

## 影响、验证与回滚

本变更不修改数据库 schema，也不改变实体 ID 或权限 key；它改变的是 provider-facing 工具目录和
session overlay。已有配置无需迁移；旧的进行中快照在恢复后应通过 loader 重建所需的延迟能力。

验证至少包括：

- `cargo fmt --all -- --check`
- `cargo test --locked -p haven-tools`
- `cargo test --locked -p haven-agent`
- `cargo clippy --workspace --locked -- -D warnings`

回滚代码即可恢复旧目录实现；如果回滚时已有快照包含新的 loader action，应先让会话完成或按测试版
策略重置快照，避免旧二进制把新 loader 选择当作未知工具。
