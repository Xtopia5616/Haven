# ADR 0741：Memory fact source 输出复用闭合集合

## 状态

已采纳并实施。

## 背景

ADR 0738 将 `list_facts.source` 请求参数收窄到 SQLite `facts.source` 的固定值 `user` / `inferred`，但 `MemoryFactResponse.source` 仍是开放字符串。请求和响应表达同一个持久分类，分立的输入 enum 与响应字符串让生成的 renderer contract 只约束一侧。`facts.source` 的数据库约束和写入校验都只允许这两个值。

`MemoryFactResponse` 是 App-owned IPC 投影，Memory repository 的 `Fact.source` 仍是领域存储文本。App 是 repository 到 renderer 的边界，负责验证该存储值符合 wire vocabulary。

## 决定

- 将 App enum 命名为 `MemoryFactSource::{User, Inferred}`，同时用于 `list_facts.source` 与 `MemoryFactResponse.source`。
- 由 Serde 保持现有小写 JSON 字面量。IPC generator 产出 `MemoryFactSourceInput`、`MemoryFactSource` 及各自的值清单，UI 筛选和 response 消费都引用生成类型。
- `MemoryFactResponse::try_from(Fact)` 严格解析 repository source；遇到不在闭合集合中的值时返回错误，不把未识别值透传到 UI。
- 保持 Memory store 查询 API 的 `Option<String>` 和持久化字段不变；筛选边界仍将 enum 映射为既有 source 文本。

## 替代方案

- 保持输入 enum 与响应字符串分立：拒绝。同一持久分类会继续在 renderer request 与 response 中呈现不同的约束。
- 由 App enum 之外新建响应专用 enum：拒绝。会重复声明同一 `user` / `inferred` 闭合集合。
- 将 enum 下移到 Memory store API：拒绝。该类型用于 App 的 Tauri request/response 投影；Memory repository 保持独立于 App IPC 的查询边界。
- 对未知 source 保持字符串透传：拒绝。数据库与写入策略已声明闭合集合，映射时 fail-closed 可避免损坏或不符合约束的数据被包装成有效 wire 值。

## 影响与验证

请求 JSON、响应 JSON 和 UI 显示的 source 字面量保持不变；TypeScript response 的 `source` 从 `string` 收窄为 `MemoryFactSource`，`list_facts.source` 统一使用 `MemoryFactSourceInput`。未知 repository source 将使对应 IPC 命令返回错误。没有数据库 schema、配置、迁移、恢复或数据重置变化。

验证：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked` 通过；UI `check`、`test:run`（125 files / 992 tests）、`build` 通过；IPC command contract 检查（80 handlers）、IPC event 检查（35 channels）、ADR index（724 records）及 `git diff --check` 通过。

## 回滚

如回滚，恢复 `MemoryFactResponse.source: String` 与 infallible mapper，并将 UI selector 恢复为既有筛选输入类型；移除此 ADR 及对应生成物和文档引用。无需修改 Memory store、数据库或用户数据。
