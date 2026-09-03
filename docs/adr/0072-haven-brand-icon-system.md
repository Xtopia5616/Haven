# ADR 0072：Haven 统一品牌图标源与状态变体

## 背景

Haven 的根图标、Tauri bundle 图标、favicon、应用内 Logo、启动占位页和
托盘图标此前不是同一个可审查的设计系统：静态入口使用旧的蓝色拱形位图，
应用内 Logo 另有一份内联 SVG，托盘则是按状态填充的纯色方块。不同尺寸和
状态下的识别度因此不稳定。

## 决定

1. 使用 `assets/branding/haven-mark.svg` 作为唯一权威视觉源：透明画布上的
   浅蓝对话气泡和三段厚声波，避免文字、底板、细线和复杂渐变。
2. 由 `scripts/generate-icons.py` 确定性生成根图标、favicon、Tauri 的 PNG、
   Windows ICO、macOS ICNS 以及 Windows Store 尺寸；提交生成后的发布资产，
   以便没有生成工具的打包环境也能直接构建。
3. `HavenMark.svelte` 作为 UI 内部的应用 Logo，使用透明画布；`StatusDot.svelte`
   保持 8×8 圆点原语，状态颜色只表达语义：未配置使用灰色 outline，录音、
   等待、生成和就绪分别沿用 error/warning/primary/success。启动占位页也保持
   灰色状态点。托盘保持现有状态事件和 tooltip 契约，保留
   normal/recording/muted/busy 语义，并将状态色作为不透明蓝色 tile 的外圈。

## 替代方案

使用 imagegen 或继续维护多份独立位图会增加不可复现的视觉漂移，不适合作为
Windows 小尺寸图标和 UI 首屏的长期源。继续使用纯色托盘方块则无法表达新品牌。

## 影响与验证

静态资源路径和 Tauri bundle 文件名保持兼容，不改变 IPC、业务逻辑或托盘状态
事件；应用内标记保持透明，桌面/任务栏静态图标使用不透明蓝色背景，动态托盘
使用不透明蓝色 tile。验证包括 SVG/PNG 尺寸、桌面图标不透明背景、ICO/ICNS
容器、四种托盘状态差异，以及 UI 检查、测试和 Tauri crate 检查。

## 回滚

回退本 ADR 对应提交即可恢复旧图标文件和旧内联标记；不需要删除用户配置、
数据库或缓存，也不涉及数据迁移。
