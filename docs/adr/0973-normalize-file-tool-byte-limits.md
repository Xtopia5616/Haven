# ADR 0973：统一文件工具字节限额与读写上限

## 状态

已采纳并实施。

## 背景

`file_read_max_chars` 用于比较文件的 `metadata.len()`，所以实际单位是字节；该限额也控制 edit/patch 的整文件载入和 patch 结果大小。`file_max_line_chars` 传给按字节保留缓冲的 bounded line reader。与此同时，`file_max_byte_read` 控制 byte-mode 单次读取和 inspect/hash 的文件扫描大小。配置与 Settings UI 把前两者描述为字符数，字段名和文档单位与实现不符。

生产装配还把 `file_max_byte_read` 复用为 `files.write`、`edit` 和 `patch` 的写入上限，因此调低读取上限会意外限制文件写入；配置中没有独立写入字段。

## 决定

1. 配置、generated Settings contract 和 UI 将整文件操作上限命名为 `file_max_full_file_bytes`，单行缓冲上限命名为 `file_line_max_bytes`，byte-mode 与 inspect/hash 读取上限命名为 `file_byte_read_max_bytes`。Rust 内部参数也按字节和操作角色命名。
2. 新增 `file_write_max_bytes`，默认 16 MiB；生产工具装配将它独立传给写入、编辑与 patch，不再从读取限额推导。Patch 源文件大小由整文件上限控制，patch 结果大小由写入上限控制；读写字段分别保留独立安全边界。
3. 三个旧配置 key 不添加 alias。配置加载按既有策略拒绝并备份旧配置；用户可手动改名并按需设置 `file_write_max_bytes`，或仅重建 `config.toml`。数据库不变。
4. 文件限额名称带 `_bytes` 或 `_chars` 后缀，文档、设置界面和实现必须使用相同单位。

## 替代方案

- 保留 `*_chars` 并修正代码按字符计量：拒绝；完整文件读取和 bounded line reader 都是字节缓冲，按字符数计量会改变其内存边界。
- 继续让读取上限同时约束写入：拒绝；二者约束不同操作，配置一个不应隐式更改另一个。
- 将旧字段名作为 Serde alias：拒绝；旧名表达了错误单位，保持 alias 会继续传播误导性配置契约。

## 影响与验证

- 默认整文件、行缓冲、byte-mode 读取与写入上限保持原有数值；写入默认值固定为原生产默认的 16 MiB，与读取字段无关。
- Rust 配置测试覆盖默认值、当前 TOML 解析/round-trip 和旧 key 拒绝；Tools 测试覆盖读取上限不影响写入及独立写入上限；UI 设置测试覆盖字段保存回环。
- 生成 IPC contract、Rust/前端检查与完整门禁通过；不改数据库或 transcript。

## 回滚

恢复旧字段与共享读取/写入上限逻辑，并同步回退 Settings contract、测试、命名与发布说明。若配置已使用新 key，回滚前需把三个重命名字段改回旧 key；数据库无需处理。
