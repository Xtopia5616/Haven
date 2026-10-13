# ADR 1131：用领域枚举表达 OCR 凭据字段

## 背景

`stage_ocr_credential` 用 `api_secret: bool` 在 OCR API key 与 API secret 两个闭合选择间分支。Common 已有私有 `OcrCredentialField`，但 IPC/UI 无法复用它，生成请求仍暴露含义不直观的布尔值。

## 决定

- 将 Common 的 `OcrCredentialField` 提升为公开、Serde snake_case 的闭合 enum：`api_key` 与 `api_secret`。
- ConfigService 与 Tauri command 接受该 enum；UI 仅传 generated `OcrCredentialFieldInput`，不再自行定义第二份词汇。
- 未知成员、旧 camelCase 字段名和布尔值均由 IPC 反序列化拒绝。

## 影响与验证

变更只影响同版本 App/UI 间的 Tauri 命令请求，不改 config.toml、Credential Manager 内容或其他持久字段，无持久数据迁移或重置要求。验证 Rust workspace、IPC contract drift、前端类型检查、测试与生产构建。

## 回滚

恢复布尔命令参数并重新生成 IPC 类型即可；配置和凭据引用不受影响。
