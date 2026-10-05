# ADR 0504：改用可用宽度选择自适应布局

- 状态：已采纳（2026-09-30）
- 关联：`docs/ui.md` §10.11（横版工作台与竖版手机布局）、[ADR 0503](0503-window-resize-aspect-ratio-bounds.md)

## 背景

现有主布局主要按视口宽高比切换。比例断点有助于判断横竖屏和超宽屏，但相同宽高比可对应完全不同的可用宽度；例如 800×450 与 1280×720 都是 16:9，前者不适合同时显示主导航、会话栏和对话画布。Android 官方的窗口尺寸级别将可用窗口宽度作为高层布局决策依据，分为 600、840、1200、1600 dp 等宽度边界；MDN 也建议在内容开始拥挤时切换布局，而不是依据设备型号或比例猜测。

## 决定

1. 顶层工作区按 WebView CSS 视口宽度选择布局，使用适配 Haven 面板几何后的 600、840、1200 CSS px 阈值：
   - `<600px`：紧凑纵向壳层，不显示常驻侧栏。
   - `≥600px`：显示 76px 图标式主导航。
   - `≥840px`：聊天显示常驻会话栏；工具、历史、记忆和设置显示分类侧栏。
   - `≥1200px`：主导航展开至 200px 并显示文字标签。
2. 21:9 宽高比继续作为超宽工作台的最大内容比例与居中规则；宽高比不再触发上述多栏布局。
3. 600/840/1200 的数值参考 Material 3 / Android Window Size Classes 的 compact、expanded、large 宽度边界，最终阈值按 Haven 的主导航、辅助侧栏和主要内容列宽组合选定。断点单位为 CSS px，不声称与 Android dp 完全等价。
4. 布局切换仍只改变展示，不改变路由、状态 owner、IPC、会话操作或交互语义。

## 影响与验证

- 默认 1280×720 窗口仍显示完整桌面布局。
- 1024×768 使用紧凑图标主导航和双栏工作区；800×600 与 768×768 使用图标主导航和单内容栏；480×720 使用窄版壳层。
- 静态审查矩阵覆盖 599/600、839/840、1199/1200 CSS px 断点两侧；Windows 桌面检查缩放和最大化时导航、会话栏、设置/工具分类栏的显隐与内容宽度。
- 无配置、数据库、Tauri IPC 或持久化语义变化。

## 替代方案

- 继续以宽高比作为所有主布局的唯一断点：实现简单且同一比例布局一致，但会让窄窗口过早进入多栏布局，因此不采用。
- 只在 21:9 断点使用宽高比，同时以宽度控制其他布局：保留适合超宽屏的内容限宽，其他版面依据真实可用宽度切换。

## 参考

- [Android Developers：Use window size classes](https://developer.android.com/develop/ui/views/layout/use-window-size-classes)
- [MDN：How to choose breakpoints](https://developer.mozilla.org/en-US/docs/Learn_web_development/Core/CSS_layout/Media_queries#how_to_choose_breakpoints)
