# ADR 1298：分离 Settings snapshot 与 patch 并要求显式媒体能力

状态：已接受并实施。

## 背景

`Settings` 同时作为全量读取响应和局部写入输入，导致调用方必须回传未编辑 section，App 再手工保护不可编辑字段。Provider 缺少 `media_capabilities` 时，Common 和 UI 分别通过 provider/host 规则推导媒体 endpoint，产生两个可变的能力真源。

## 决定

1. 使用 `SettingsSnapshot` 表达去除凭据后的完整读取响应；使用 `SettingsPatch` 表达可省略的设置 section。App 只应用实际提交的 section，UI 对比已加载 snapshot，仅提交变化 section。section 内 `Option` 字段保留 `null` 清除语义。
2. `ProviderConfig.media_capabilities` 必须含 `stt`、`tts`、`image_gen` 三个显式布尔值。Provider 默认和所有 UI 预设都提供完整值；Common 与 UI 删除根据 provider/host 推断的缺省路径。API style validator 仍限制 adapter 可支持的最大能力。
3. TOML 保留当前 sparse config 的其它 defaults；仅缺失 media capabilities 属于无效当前 schema。已有缺字段文件进入既有备份与只读恢复流程，用户从备份补齐字段或用当前模板重建配置。保留 ConfigService 原子保存、失败时恢复内存快照、外部编辑 fingerprint、Settings runtime partial apply 以及敏感值保护。
4. Tauri 的响应/请求 DTO 与生成 TypeScript 分别对应 `SettingsSnapshot` / `SettingsPatch`。数据库 schema 和用户数据库不受影响；无需数据库 reset。本次不执行真实用户配置修改或重建。

## 影响与恢复

- 缺字段的旧 `config.toml` 会被备份并触发只读恢复，不会将默认值写回覆盖原文件。修复备份或删除配置后重启即可恢复可写；后者会重建默认设置，不涉及数据库。
- 回退到不认识 required media capabilities 的旧二进制时，需要从配置副本中移除该字段；该回退路径不属于当前 schema 的自动兼容契约。

## 验收

- 测试覆盖省略 section 保持不变、nullable 字段显式清空、media capabilities 缺失拒绝、配置持久化失败不应用内存快照，以及 Settings 的现有 partial runtime apply 行为。
- IPC 生成与漂移检查、Rust workspace locked tests/严格 Clippy、UI check/tests/build 通过。
- 当前用户数据库和配置重置均未执行。
