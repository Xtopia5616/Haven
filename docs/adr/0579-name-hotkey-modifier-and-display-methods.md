# ADR 0579：明确 Hotkey 修饰键判断与显示名称

## 状态

已采纳并实施。

## 背景

`KeyCombo::has(u8)` 接收修饰键位掩码并返回是否设置该位；名称没有标明判断对象。`KeyCode::name()` 返回的是格式化给用户看的标签（例如 `ArrowLeft` 显示为 `Left`、字母显示为大写），并非输入 token 或稳定键身份。App 的平台 shortcut adapter 跨 crate 调用 `has`；该显示标签由 `KeyCombo` 的 `Display` 和 Tools simulation key mapping 使用。

## 决定

1. 将 `KeyCombo::has` 命名为 `has_modifier`。
2. 将 `KeyCode::name` 命名为 `display_name`。
3. 保持 key parsing、modifier bit 值与快捷键显示格式不变。

## 替代方案

- 保留 `has` / `name`：拒绝，两个名字都依赖类型上下文才能知道检查或返回什么。
- 将显示标签称为 `canonical_name`：拒绝，当前返回值包含产品显示映射（例如 Left），不保证等于 parser 的 canonical input token。

## 影响与验证

- 重命名 Haven 内部 Rust API 和测试引用；跨 crate 的 App adapter 同步到 `has_modifier`，Tools simulation 同步到 `display_name`。
- 不改变用户快捷键设置字符串、Tauri payload 或 Windows shortcut 映射。
- 验证：Rust `fmt --check`、workspace `check`、严格 Clippy、workspace serial tests、ADR 索引与差异空白检查。

## 回滚

将 `has_modifier` 与 `display_name` 恢复为原方法名，并还原 App adapter、测试与命名文档。
