# ADR 0729：模型配置命令复用 RequestKind 路由

## 状态

已采纳并实施。

## 背景

`switch_model`、`set_reasoning_effort` 与 `set_web_search` 的生产调用方都在聊天 toolbar，实际操作目标是逻辑请求路由 `chat`。但后端使用名为 `role: String` 的参数：`switch_model` 将其解析为 `RequestKind`，参数命令则通过 `model_id_for_selector` 同时接受模型配置 ID 或 `RequestKind`。同一个自由字符串因此可以表示消息身份、路由身份或模型配置身份，且参数命令可以绕过当前路由直接修改指定模型。

## 决定

- 三条命令都以 `request_kind: RequestKind` 作为路由选择器，Tauri/UI wire 字段为 `requestKind`。
- `switch_model` 仍以独立 `model_id` 选择要分配到该路由的模型配置；能力与配置完整性继续在保存前校验。
- `set_reasoning_effort` 与 `set_web_search` 只更新该路由当前分配的模型。未配置路由时返回明确错误；内置搜索仍校验当前模型 provider 的 wire capability。
- 删除同时接受模型 ID 与请求类别的字符串解析器；消息 `role`、模型 `request_kind` 和配置 `model_id` 保持不同名词与类型。

## 替代方案

- 保留 `role: String` 并只改 UI 调用：拒绝。后端契约仍允许两种身份含义并且生成类型无法表达闭合集合。
- 统一改为 `model_id`：拒绝。生产操作绑定的是当前请求路由，UI 不应缓存并传入可能过期的模型分配身份。
- 将参数命令拆成按配置 ID 的新命令：拒绝。当前没有生产调用方需要绕开请求路由直接编辑命名模型配置。

## 影响与验证

命令 Rust 参数从 `role: String` 改为 `request_kind: RequestKind`；UI 传 `requestKind` 并由生成的 `RequestKindInput` 限定值域。更新调用点、测试、命令安全元数据、IPC 文档、命名规范与架构路线图。不改变 TOML 持久结构、数据库或 provider wire；旧 IPC 字段不保留兼容别名。

验证：Rust workspace fmt/check/Clippy/tests、UI check/tests/build、IPC 生成与漂移检查、事件检查、ADR 索引及 diff checks。

## 回滚

恢复三个 handler 的自由 `role: String`、旧 UI payload 与 selector helper 即可；无持久数据迁移。
