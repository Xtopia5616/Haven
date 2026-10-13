# ADR 1132：将 Credential Manager target 规范为小写

## 背景

Credential Manager 的 machine target 被写作 `Haven/{credential_ref}`，与本项目“机器比较、拼接或写入存储时使用小写 `haven`”的命名规则不一致。该 target 由规范 `cred-*` 引用派生，不能改变引用本身。

## 决定

- 统一使用 `haven/{credential_ref}` 作为 Credential Manager target。
- 不增加双读、复制或重置流程：Windows 通用凭据的 `TargetName` 大小写不敏感，`CredReadW`、`CredWriteW` 与 `CredDeleteW` 使用该 target 时仍指向现有同一条凭据。新写入和删除路径自然采用规范小写。

微软将通用凭据的 `TargetName` 说明为大小写不敏感，见 [CREDENTIALW](https://learn.microsoft.com/en-us/windows/win32/api/wincred/ns-wincred-credentialw)。

## 影响与验证

凭据 reference、秘密值、读取/写入权限和 config.toml 均不变；不需要迁移或重置。Windows 单元测试检查生成的 target 名称为小写 namespace。不得在测试中读写真实用户 Credential Manager。

## 回滚

将 target prefix 恢复为 `Haven/` 即可；由于 Windows target 大小写不敏感，无需清理持久数据。
