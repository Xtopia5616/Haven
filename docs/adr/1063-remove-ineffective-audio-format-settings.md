# ADR 1063：移除无效音频格式配置并统一录音范围

## 状态

已接受（2026-10-12）

## 背景

`AudioConfig` 与 Settings 曾暴露采样率、声道数和位深，但 `haven-input` 不读取这些值。采集端由设备选择实际输入格式，再转成单声道并重采样；录音 WAV 固定输出 16 kHz、单声道、16-bit PCM。保留这些无效字段会让用户误以为设置能改变输入或输出格式。

录音最大时长和静音超时在 UI 有控件范围，Common 配置与 Settings command 边界却没有语义范围。非法配置可能导致录音立即停止，或偏离用户在设置界面可输入的范围。

## 决定

- 移除 `AudioConfig.sample_rate`、`channels` 和 `bits_per_sample`，在设置页面明确展示固定 WAV 输出格式。
- 统一 `max_duration_secs` 为 10–300 秒，`silence_timeout_ms` 为 500–10,000 毫秒；Common 对 TOML 和 Settings DTO 校验，UI parser 与控件使用同一数值范围。
- 保持输入设备格式自动选择；输出格式由 `RecordingResult` 保持固定，不引入额外可配置格式层。
- 旧配置不做字段迁移。`deny_unknown_fields` 将拒绝包含已删除字段的 `[media.audio]`；发布说明指导用户删掉三行并保留其它配置。

## 影响

设置只呈现当前实际生效的音频行为；配置、IPC Settings 校验和 UI 控件对录音参数有一致边界。含旧格式字段的 `config.toml` 需按发布说明手动编辑或重建，不影响数据库。

## 验证

- Common 测试覆盖 AudioConfig 拒绝旧格式字段，并覆盖 duration/silence 配置与 Settings 边界。
- UI contract 测试覆盖两项范围和只含有效配置的生成形状；Input 测试断言 WAV 为 16 kHz、单声道、16-bit PCM。
- Rust workspace、UI、IPC contract generator 和 ADR 链接检查通过。
