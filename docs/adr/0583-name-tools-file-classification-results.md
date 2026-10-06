# ADR 0583：为 Tools 文件与受管媒体分类使用具名结果

## 状态

已采纳并实施。

## 背景

Tools 内部有两条不同的分类路径。`files` 按路径扩展名分类文件处理类别（image、archive、executable 等）并关联 MIME 类型；managed-media 路径按资产 MIME/受控文件名推断媒体种类，并生成现有 `file_kind` 标签。两者原先都返回位置 tuple，调用方通过 `.0` / `.1` 读取，无法从访问表达式直接看出值的角色。

两条分类的值域与消费者不同：文件处理类别包含 archive、executable、PDF 和 Office 文档；受管媒体分类服务于媒体能力判断和媒体引用投影。因此不把它们合并为一个 enum 或分类 owner。

## 决定

1. 文件扩展名分类返回 `FileClassification { file_kind, mime_type }`，类别由 `FileClassificationKind` 表示。
2. 受管资产分类返回 `ManagedMediaClassification { media_kind, file_kind }`。
3. 保留两条分类路径的独立 owner 与行为，不为相似外形合并分类空间。
4. 所有调用点改用具名字段。工具输出中的 `file_type`、`mime`、`modality`、`file_kind` JSON key 和既有值保持不变。

## 替代方案

- 继续使用 tuple：拒绝，跨模块的稳定分类语义只能通过位置辨认，调用处不够清晰。
- 合并两个分类 enum：拒绝，文件处理类别和媒体能力分类代表不同的值域与决策职责。
- 统一使用单一 `kind` 字段：拒绝，调用方容易将文件处理 kind 与媒体 kind 混淆；字段名应标出其领域角色。

## 影响与验证

- 更新 Tools 的文件读取、二进制提示、媒体 handoff、媒体引用与受管媒体操作调用点，以及现有分类测试。
- 更新 `docs/naming.md`，要求稳定的跨模块多值领域结果使用具名字段。
- 不改变 MIME 识别规则、文件类别、媒体能力选择、handoff 路由或工具 JSON 输出；无持久化、配置、IPC 或安全契约变化，无需数据库/配置重置。
- 验证通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --locked -p haven-tools -- --test-threads=1`（799 个单测通过、7 个集成测试通过、2 个忽略）、ADR 索引及 `git diff --check`。额外的 `--all-targets` Clippy 扫描命中本次未修改的 Tools 测试文件中的既有 lint；仓库标准 workspace Clippy 门禁通过。

## 回滚

将两个具名结果恢复为原位置 tuple，并同步恢复调用点；外部 JSON 契约无需变更。
