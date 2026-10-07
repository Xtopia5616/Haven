# ADR 0738：Fact source 筛选复用持久化闭合集合

## 状态

已采纳并实施。

## 背景

`list_facts.source` 只由 Memory 页面调用，页面的固定选择是全部、`user`、`inferred`。事实表 `facts.source` 有 SQLite `CHECK(source IN ('user','inferred'))`；写入验证也只允许这两个值。UI 当前把全部映射为 `null`，把两个具体选择作为字符串发送。

Tauri handler 原先接受 `Option<String>`，因此 IPC 接受任意 source 文本。未知值虽然不会匹配行，但把有约束的持久分类暴露成开放筛选字符串。内部 `MemoryFactStore::list_facts` 仍以 `Option<String>` 查询数据库，并将空字符串视为全部来源。

## 决定

- App IPC 声明 `FactSourceFilter::{User, Inferred}`，`list_facts` 接受 `Option<FactSourceFilter>`。它生成 UI 使用的 `FactSourceFilterInput`，且只允许与数据库约束相同的两个值。
- handler 在 App 到 Memory store 的边界把 enum 映射为既有 `user` / `inferred` 文本。store 查询与过滤、排序、敏感事实过滤保持原样。
- `None`（Tauri JSON `null` 或省略）继续列出所有来源；具体值筛选对应来源。空字符串及未知值在 IPC 输入处被拒绝。
- UI 页面和 `MemoryCenter` 的筛选 state、选项及回调引用生成 enum；来自 select 的字符串先按生成值清单校验。
- `MemoryFactResponse.source` 继续使用当前 wire 字符串字段，本 ADR 只收窄查询选择器，不改变响应 DTO、数据库写入、schema 或事实显示。

## 替代方案

- 保留 `Option<String>`：拒绝。SQLite schema、写入 validator 和唯一 renderer 已证明筛选值域封闭。
- 把空字符串作为“全部”枚举成员：拒绝。现有 IPC 表达已使用 `null`；空选择通过 null 传输，继续让空字符串等同于省略会保留多余输入状态。
- 把 Memory store 查询接口也改为 App enum：拒绝。App enum 属于 Tauri input contract；Memory store 作为领域服务继续接收 source string，不依赖 App crate。

## 影响与验证

Generated request 从 `source?: string | null` 收窄为 `source?: FactSourceFilterInput | null`。已有 UI payload 与 query semantics 不变；空字符串和未声明的 source 值由 Tauri 反序列化拒绝。响应字段与事实表值不变。无 schema、配置、迁移、恢复或数据重置变化。

验证：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked` 通过；UI `check`、`test:run`（125 files / 992 tests）、`build` 通过；IPC command contract 检查（80 handlers）、IPC event 检查（35 channels）、ADR index（721 records）及 `git diff --check` 通过。

## 回滚

如回滚，恢复 `list_facts` 的 `Option<String>`、UI 开放筛选类型和 generated request，删除本 ADR 与路线图/命名/IPC 文档更新；无需修改 Memory store、数据库或重置数据。
