# 0951：校验 Settings response 中的 credential references

## 状态

已接受并实现（2026-10-11）。

## 背景

`get_settings` 通过 `ConfigService::settings()` 返回安全化 Settings DTO。Provider、OCR 与 MCP environment 的 credential reference 会保留在返回值中，Settings 页面随后会把这些值放进本地配置草稿并在保存时提交。Rust 配置 owner 已在启动和保存时校验引用，但 renderer 的 `parseSettingsPayload` 之前只检查顶层对象。

## 决定

- Settings response parser 校验 `llm.providers[].api_key_ref`、`media.ocr.api_key_ref`、`media.ocr.api_secret_ref` 和 `mcp_servers[].env_refs[].credential_ref`。
- 每个引用接受省略或 `null`；非空值必须是规范 `cred-*` ID。
- 保留现有 Settings 顶层 shape 容错与额外字段透传，不在此处重复实现完整配置 schema，也不改变 Rust 生成的 IPC DTO。

## 影响

无效的持久 credential reference 不会进入 Settings 草稿或后续保存请求。合法配置、credential store 和持久化语义不变，无需重置配置或凭据。

## 验证

- `corepack pnpm run check`
- `corepack pnpm run test:run`
- `corepack pnpm exec prettier --write` 涉及的 UI 文件
- `git diff --check`

## 回滚

若 Settings contract 新增 credential reference 字段，应在开放给 renderer 使用前纳入同一 `cred-*` 校验；不要移除已有字段校验而让持久 reference 重新以任意字符串进入草稿。
