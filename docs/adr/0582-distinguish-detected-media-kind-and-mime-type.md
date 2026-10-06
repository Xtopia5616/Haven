# ADR 0582：区分媒体粗分类与 MIME 类型

## 状态

已采纳并实施。

## 背景

Common `MediaType` 表示 Text/Image/Audio/Video/Document/Unknown 粗分类；`detect_media_type` 却返回 `image/png` 这类 MIME 字符串。`media_type_from_extension` 返回 MIME 字符串，`media_type_from_mime` 又返回粗分类，转换方向难以从方法名判断。`MediaProbe.media_type` 也存放粗分类，容易与 `mime_type` 混淆。

媒体能力规划中的 `MediaModality` 与检测结果分类虽共享多数已知类别，但前者用于模型能力合同且没有 `Unknown`，后者必须表达探测失败。两者不是同一个约束，不应仅因枚举值相似而合并。

## 决定

1. 将粗分类 enum 命名为 `DetectedMediaKind`，将 `MediaProbe.media_type` 字段命名为 `media_kind`。
2. MIME 检测和 extension 映射函数统一命名为 `detect_mime_type`、`detect_mime_type_with_filename`、`mime_type_from_extension`、`extension_for_mime_type`。
3. MIME 到粗分类的转换命名为 `media_kind_from_mime_type`；bytes/filename 粗分类检测命名为 `detect_media_kind`。
4. `MediaModality` 保留给能力规划合同；`DetectedMediaKind::Unknown` 不转入该能力集合。
5. `DetectedMediaKind::is_rich` 改为 `is_rich_media`。
6. `MediaReference.modality` 的序列化字段和值保持不变；它的 Rust 类型改为 `DetectedMediaKind`。
7. 既有持久或 wire MIME 字段（如 `media_type`）保持兼容。`MediaProbe.media_kind` 的 Serde key 保持旧值 `media_type`，也没有生产 Tauri/tool payload 消费该 probe serialization shape。

## 替代方案

- 将粗分类继续叫 `MediaType`：拒绝，MIME 标准术语和 `mime_type` 字段已经占用“media type”含义。
- 将 MIME 字符串变量也叫 `MediaType`：拒绝，字符串是完整 MIME 类型，粗分类是另一层映射。
- 合并 `MediaModality` 与检测 enum：拒绝，`Unknown` 探测失败与能力合同里的 modality 不等价。

## 影响与验证

- 同步 Common re-export、App attachment ingress、Tools media/file consumers 和跨层输出清单。
- MediaProbe 仅为 internal typed probe；Rust 字段名变化而 Serde key 保持兼容。持久/wire MIME 字段、MediaReference 的 JSON 字段和值、媒体探测顺序及工具行为不变。
- 验证：Rust `fmt --check`、workspace `check`、严格 Clippy、workspace serial tests、ADR 索引与差异空白检查。

## 回滚

将 `DetectedMediaKind`、`media_kind` 字段和 MIME helper 恢复为旧名，并同步还原 App/Tools 调用点与命名文档。
