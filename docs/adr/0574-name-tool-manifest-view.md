# ADR 0574：区分工具 manifest wire 与 UI 视图

## 状态

已采纳并实施。

## 背景

generated IPC contract 的 `ToolManifest` 是 Rust snake_case wire DTO。UI `toolManifest.ts` 中同名 type 则是 parser 产出的 camelCase renderer view，required values 经验证，来源字段用开放 `ToolManifestSource` 以保留未来值。`builtinToolPresentation.ts` 也消费这份 view。相同类型名因此掩盖了不同字段 casing、约束和边界 owner。

## 决定

1. renderer shape 改名为 `ToolManifestView`；generated `ToolManifest` 保持唯一 wire DTO 名称。
2. parser、manifest cache/accessor 的返回类型与 Settings 投影依赖一律使用 `ToolManifestView`。
3. `ToolManifestSource` 继续表示 parser 接受的开放字符串；闭合来源分类使用 generated `ToolSource`。

## 替代方案

- 将 UI parser 收窄为 strict generated `ToolManifest`：拒绝，UI parser 需要把未知来源值保留为开放字符串并转换 snake_case 到 camelCase。
- 让 generated DTO 改名为 `ToolManifestWire`：拒绝，generated type 明确代表后端 IPC DTO，其 owner 已清晰；应标明额外 renderer projection 的角色。
- 合并 wire DTO 与 view：拒绝，会把 runtime validation、字段 casing 与前端展示 shape 混入 Rust generated contract。

## 影响与验证

- 仅重命名 UI 内部 renderer view 类型及其 Settings consumer；parser、字段映射、manifest values 和展示行为不变。
- 无 Rust、Tauri wire、数据库或配置契约变化。
- 验证：UI `check`、`test:run`、`build`、ADR 索引与差异空白检查。

## 回滚

将 `ToolManifestView` 恢复为 `ToolManifest`，并同步还原其唯一 Settings consumer 与命名文档。
