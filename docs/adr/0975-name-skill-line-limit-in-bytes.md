# ADR 0975：用字节命名 SKILL.md 单行上限

## 状态

已采纳并实施。

## 背景

`skills_max_line_len` / `max_line_len` 的实现使用 `str::len()`，比较的是 UTF-8 字节数；设置页和超限错误却称其为字符。多字节文本因此会比用户理解的字符限额更早被拒绝，而现有 ASCII 测试无法揭示单位差异。

## 决定

1. 配置字段改为 `skills_max_line_bytes`，解析参数改为 `max_line_bytes`，设置页单位和错误文本均明确为 UTF-8 bytes。4096 的默认值和按字节限制的行为保持不变。
2. 不将实现改为 Unicode 字符计数，保留现有按 UTF-8 字节限制输入行长度的资源边界。
3. 旧配置 key 不添加 alias。需保留现有数值时，将 `[context_limits].skills_max_line_len` 手动改为 `skills_max_line_bytes`；数据库不变。

## 替代方案

- 改为按 Unicode 字符数计量：拒绝；这会放宽多字节行占用的内存上限，且改变已有解析行为。
- 保留旧 key，仅修正文案：拒绝；字段名仍不能表达解析器实际比较的单位。

## 影响与验证

- 配置默认值、解析器门槛和输入处理行为不变；生成的 Settings contract、设置页、错误信息及配置迁移说明使用字节单位。
- Rust 测试覆盖当前 key、旧 key 拒绝、ASCII 超限与多字节 UTF-8 超限；Settings 测试覆盖该字段保存回环。
- 不改数据库或 Skill 文件格式。

## 回滚

恢复 `skills_max_line_len` 和字符文案，回退 Settings contract、测试、发布说明与 ADR。使用新 key 的配置在回滚前应改回旧 key；数据库无需处理。
