# 0950：校验 Settings 返回的 staged credential reference

## 状态

已接受并实现（2026-10-11）。

## 背景

`stage_provider_credential` 与 `stage_ocr_credential` 将秘密值写入 credential store，并返回 opaque `cred-*` 引用。Settings 页面直接把返回字符串写入配置草稿，后续随 Settings update 持久化。命令 adapter 之前只依赖生成类型 `string`，没有校验运行时身份格式。

## 决定

- Settings contract 校验两个 staging response 都是规范 `cred-*` ID。
- 两个 command adapter 在返回引用前执行同一 parser；格式异常时拒绝 response，页面不会把无效引用写入草稿。
- Secret value 仍只通过 staging request 进入 Rust，错误信息不包含 secret；credential store 与 Settings wire DTO 不变。

## 影响

合法凭据 staging、保存、credential-store 生命周期和 IPC shape 不变。该校验仅限制 command response 的 opaque entity identity，无需重置配置或凭据。

## 验证

- `corepack pnpm run check`
- `corepack pnpm run test:run`
- `corepack pnpm exec prettier --write` 涉及的 UI 文件
- `git diff --check`

## 回滚

若 Credential Manager 引用格式变化，先统一更新 ID 规范、Rust producer、生成 contract 与 parser 测试；不要直接将 staging command 的任意字符串写入 Settings 草稿。
