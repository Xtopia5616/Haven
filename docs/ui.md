# Haven UI 设计规范与编码规则

> 版本: v2.3 | 日期: 2026-10-20
>
> 本文定义 Haven UI 设计系统、信息架构和交互契约。组件与页面的具体实现以当前代码为准。

---

## 目录

1. [设计系统](#1-设计系统)
2. [组件约定](#2-组件约定)
3. [CSS 命名与样式规则](#3-css-命名与样式规则)
4. [状态管理](#4-状态管理)
5. [事件处理](#5-事件处理)
6. [Tauri 桥接](#6-tauri-桥接)
7. [可访问性](#7-可访问性)
8. [文件与目录结构](#8-文件与目录结构)
9. [Svelte 5 语法规则](#9-svelte-5-语法规则)
10. [UX 信息架构与交互契约](#10-ux-信息架构与交互契约)

---

## 1. 设计系统

基于 **Material Design 3 Expressive** token 系统，所有 token 定义在 `ui/src/app.css`。

### 1.1 颜色系统

使用 CSS 自定义属性，通过 `[data-theme="light|dark"]` 切换。

| Token 类别 | 命名模式 | 示例 |
|---|---|---|
| 主色 | `--md-sys-color-primary` | `#3378D6` |
| 主色上文字 | `--md-sys-color-on-primary` | `#ffffff` |
| 主色容器 | `--md-sys-color-primary-container` | `#d7e3ff` |
| 主色容器上文字 | `--md-sys-color-on-primary-container` | `#001b3e` |
| 次要色 | `--md-sys-color-secondary` | ... |
| 第三色 | `--md-sys-color-tertiary` | ... |
| 错误色 | `--md-sys-color-error` | `#ba1a1a` |
| 成功色 | `--md-sys-color-success` | `#2e7d32` |
| 警告色 | `--md-sys-color-warning` | `#7d5700` |
| 背景/表面 | `--md-sys-color-surface` | ... |
| 表面变体 | `--md-sys-color-surface-variant` | ... |
| 轮廓线 | `--md-sys-color-outline` | `#777680` |
| 轮廓线变体 | `--md-sys-color-outline-variant` | `#c7c5d0` |

**规则**:

- 不允许在组件样式中硬编码颜色值，必须使用 CSS 变量
- 不允许使用 `color-mix()` 之外的颜色函数直接操作颜色值
- 组件变体使用 `data-variant` 属性驱动主题切换

蓝色强调色保留手工调校的默认配色；其他预设和自定义强调色以所选色为种子，在浅色/深色模式下共同派生主色、次要色、第三色和表面轻染色，避免沿用蓝紫色容器。强调色及容器文字的对比色由 themeStore 按当前主题计算；状态色映射到主色、成功、警告、错误和轮廓等语义 token。应用内 HavenMark 使用主色容器及其文字 token，随强调色和浅/深色模式更新；安装包图标继续使用固定品牌配色。

### 1.2 字体与行高

字号、字重和行高使用语义 token，禁止页面随意组合字号与 `line-height`。正文默认使用 14px / 1.55；小号正文使用 13px / 1.5；按钮和标签使用 14px / 1.4 或 12px / 1.45；标题按层级使用 18px、20px、28px 和 30px，并保持至少 1.2 的行高。

| Token | 值 | 适用场景 |
|---|---:|---|
| `--md-sys-typescale-display-size` / `-line-height` | 30px / 1.2 | 欢迎页主标题 |
| `--md-sys-typescale-headline-large-size` / `-line-height` | 28px / 1.25 | 页面主标题 |
| `--md-sys-typescale-headline-medium-size` / `-line-height` | 20px / 1.3 | 区块标题 |
| `--md-sys-typescale-title-large-size` / `-line-height` | 18px / 1.35 | 会话标题 |
| `--md-sys-typescale-body-medium-size` / `-line-height` | 14px / 1.55 | 默认正文、控件与辅助文本 |
| `--md-sys-typescale-body-large-size` / `-line-height` | 15px / 1.55 | 聊天回复等连续阅读正文 |
| `--md-sys-typescale-body-small-size` / `-line-height` | 13px / 1.5 | 辅助正文 |
| `--md-sys-typescale-label-medium-size` / `-line-height` | 12px / 1.45 | 状态、菜单、次要标签 |
| `--md-sys-typescale-label-small-size` / `-line-height` | 11px / 1.4 | 极少量元数据 |

### 1.3 形状系统

| Token | 值 | 适用场景 |
|---|---|---|
| `--md-sys-shape-extra-small` | 4px | 复选框、微小组件 |
| `--md-sys-shape-small` | 8px | 微小按钮、菜单项、chip |
| `--md-sys-shape-medium` | 12px | Tab、统一按钮和控制条 |
| `--md-sys-shape-large` | 16px | Section 容器 |
| `--md-sys-shape-extra-large` | 28px | Dialog |
| `--md-sys-shape-full` | 9999px | 全圆角 |

### 1.4 间距系统

| Token | 值 |
|---|---|
| `--md-sys-space-xs` | 4px |
| `--md-sys-space-sm` | 8px |
| `--md-sys-space-md` | 12px |
| `--md-sys-space-lg` | 16px |
| `--md-sys-space-xl` | 20px |
| `--md-sys-space-2xl` | 24px |
| `--md-sys-space-3xl` | 32px |
| `--md-sys-space-4xl` | 48px |

### 1.5 动效系统

| Token | 值 | 适用场景 |
|---|---|---|
| `--md-sys-motion-easing-standard` | `cubic-bezier(0.2, 0, 0, 1)` | 通用过渡 |
| `--md-sys-motion-easing-emphasized` | `cubic-bezier(0.3, 0, 0, 1)` | 弹窗、菜单 |
| `--md-sys-motion-duration-fast` | 150ms | 颜色变化、状态层 |
| `--md-sys-motion-duration-short` | 300ms | 通用过渡 |
| `--md-sys-motion-duration-medium` | 450ms | 布局变化 |
| `--md-sys-motion-duration-long` | 700ms | 较长的强调动效 |

### 1.6 阴影层级

| Token | 适用场景 |
|---|---|
| `--md-sys-elevation-0` | 默认 |
| `--md-sys-elevation-1` | 悬浮卡片、按钮 hover |
| `--md-sys-elevation-2` | 卡片 hover |
| `--md-sys-elevation-3` | 下拉菜单、Snackbar |
| `--md-sys-elevation-4` | Dialog |
| `--md-sys-elevation-5` | 最高层级 |

### 1.7 组件原始类

定义在 `app.css` 中的全局组件类：

| 类名 | 用途 | 变体 |
|---|---|---|
| `.md-btn` | 按钮 | `--filled`, `--tonal`, `--elevated`, `--outlined`, `--text`, `--success`, `--warning`, `--danger`, `--danger-text`, `--xs` |
| `.md-icon-btn` | 图标按钮 | `data-variant`、`data-size` |
| `.md-toolbar` | 统一工具栏的对齐与间距 | — |
| `.md-input` | 文本输入框 | - |
| `.md-textarea` | 多行输入 | - |
| `.md-card` | 卡片容器 | `--elevated`, `--outlined` |
| `WorkspaceSurface` | 次级工作区的统一背景与入场动效 | `entering` |
| `.md-chip` | 标签 chip | - |
| `.md-divider` | 分割线 | - |
| `.md-tabs` / `.md-tab` | Tab 导航 | `active` |
| `.md-badge` | 状态徽章 | `data-variant` 属性 |
| `.md-slider` | 滑块 | - |

---

## 2. 组件约定

### 2.1 Props 定义

所有组件使用 Svelte 5 `$props()` 解构语法，禁止 `export let`：

```svelte
<script lang="ts">
 let { value = '', min = undefined, max = undefined, onChange, id = undefined } = $props();
</script>
```

**规则**:

- 为每个 prop 提供默认值
- 回调 prop 使用 `on` 前缀命名（`onChange`, `onClick`, `onClose`）
- 回调调用使用可选链 `onChange?.(val)`
- 使用 JSDoc 注释说明 prop 的用途和类型

### 2.2 Children / Snippets

容器组件使用隐式 `children` snippet：

```svelte
<script lang="ts">
 let { children } = $props();
</script>
{@render children?.()}
```

消费方使用 `{#snippet}` 传递命名 snippet：

```svelte
<MaterialDialog {open} onClose={...} title="...">
 {#snippet children()}
  <p>Content</p>
 {/snippet}
 {#snippet footer()}
  <button>Cancel</button>
  <button>Confirm</button>
 {/snippet}
</MaterialDialog>
```

### 2.3 组件文件模板

```svelte
<script lang="ts">
 /**
  * ComponentName — 简短描述
  * @prop {type} propName — 描述
  */
 let { prop1 = default, prop2, children } = $props();

 let localState = $state(false);
</script>

<!-- 模板 -->

<style>
 /* 组件样式 */
</style>
```

### 2.4 组件职责边界

| 组件类型 | 职责 | 禁止 |
|---|---|---|
| **Route / App shell** | 路由装配、启动与跨域应用生命周期；可直接执行由该层独占编排的跨域启动/生命周期命令，且必须登记在 IPC owner 门禁中 | 直接操作 DOM；把 feature draft 或领域状态机堆入 shell；直接发起已有领域 command adapter 拥有的动作 |
| **`lib/views/` feature view** | 单一领域的加载、编辑草稿和页面交互；经领域 `*Commands.ts` 发起 Tauri 命令 | 直接调用 `invoke`；复制 command adapter 的请求/响应处理 |
| **全局投影宿主** | 由 AppShell 单例挂载，订阅对应全局投影并呈现，例如通知与 context menu host | 成为投影数据的第二写入 owner |
| **UI 行为组件** | 拥有局部输入状态，通过 callback/controller port 请求动作，例如 Composer | 直接调用 `invoke`；拥有跨页面生命周期 |
| **普通可复用组件 / renderer** | 展示 props、局部交互和视觉结构 | 直接调用 `invoke`、读取全局 store、加载领域页面数据 |
| **领域 command adapter (`*Commands.ts`)** | 按领域封装生成契约对应的 `invoke`，并在必要时验证响应 | 持有 Svelte 页面状态或决定用户可见通知文案 |

领域 view 把动作交给 command adapter；复用组件把动作交给 owner callback。闭合的 Tauri request/response 直接来自 `generatedCommands.ts`，不在组件 props 或 view 内重新声明 wire shape。

`WorkspaceSurface` 为任务、工具、记忆和设置等次级工作区提供统一背景与工作区切换时的入场动效；页面标题、筛选器、列表和详情内容由各工作区自己提供。容器分层优先使用背景色与留白，避免工作区外框、分类导航和内容卡片连续叠加描边。对话工作区保持全宽布局，不套用此外框。工具、历史和设置统一使用填满可用高度的工作区背景与底部留白，不再由设置页单独补高度；分类内容按“区块标题 → 搜索/筛选 → 内容”的顺序排列。分类标题与说明复用 `WorkspaceSectionHeader`；搜索框复用 `WorkspaceSearch`，清空后焦点返回输入框。筛选和操作按钮按内容宽度或共享控件宽度排列，窄内容区允许换行，不用固定像素宽度截断日期范围。

聊天画布最大宽度为 960px，消息列、输入框及返回底部按钮共享水平边距，随对话区实际宽度在 12–32px 之间连续变化。正文使用 14–15px 的流式字号，会话标题使用 16–18px，均以 rem 为基础并通过容器单位适配；消息间距与卡片水平留白在 12–16px 之间变化。会话顶部、输入工具栏和会话栏新建按钮沿用全局共享控件尺寸（默认 40px），会话列表行最低 56px；列表标记与结束提示沿用 compact 尺寸，工具图标沿用 dense 尺寸。工具记录使用 13px 次级正文并收紧行间距，结束提示将原因标签与正文并排，降低辅助卡片的视觉占比。普通助手回复、工具记录、结束提示与输入框共享同一内容列和左右边界；用户短消息按内容收宽，长消息最多占阅读区的 88%（窄窗口 96%），工具结果和代码/表格可使用完整聊天画布宽度。超宽窗口保留阅读列上限，不按窗口高度裁窄应用外框。

多行输入最高为视口高度的 28% 且不超过 240px；高度低于 560px 时最高为视口高度的 22% 且不超过 88px，单行最低高度从 48px 调整为 40px。输入框自动增长读取 CSS 最低高度，并在宽度或窗口高度变化后重新计算；附件预览横向滚动，避免持续挤占阅读区。模型菜单高度不超过视口一半，靠发送按钮一侧对齐，窄菜单的选项允许换行。会话操作按钮保持文字并在窄窗口等分或换行；长标题省略时通过悬停显示全文。

带图标的横向 Tab 在自身内容宽度不超过 72px 时隐藏文字与辅助说明，保留图标、选中指示、完整可访问名称和悬停提示。主导航在窗口宽度 600–900px 时使用 72px 图标栏，宽窗口恢复文字；无图标的 Tab 保留文字与既有滚动行为。

### 2.5 已收敛的共享组件

以下组件是跨页面复用的首选入口；页面只保留业务编排和少量布局覆盖，不再为相同语义重复实现按钮或控件外观：

| 组件 | 统一的交互 | 使用边界 |
|---|---|---|
| `MaterialButton` | filled / tonal / outlined / text / success / warning / danger / danger-text 按钮、加载态、展开态和 toggle 状态 | 文字动作和组合按钮的主动作 |
| `MaterialIconButton` | 关闭、复制、编辑、显隐、刷新等图标动作；紧凑行内删除可用 `danger-text` | 仅用于符合下方选择规则的纯图标动作，必须提供 `label` |
| `MaterialSelect` | 具名的选择型 combobox、分组 listbox、鼠标和键盘选择 | 设置、筛选及其它有限选项集合 |
| `MaterialChoiceChip` | 可选择的紧凑选项 | 选项集合；支持键盘回车提交等页面回调 |
| `CountChip` | 工作区筛选栏和资源列表的数量提示 | 统一展示“共 X …”计数，不承载操作 |
| `MaterialTabs` | 工作区和设置页 Tab 导航 | 统一 tablist、选中态和指示器 |
| `MaterialCollapsible` | 分组、详情和辅助信息的统一展开/收起控件 | 所有区块级折叠内容；通过 `header` snippet 提供标题、状态或计数 |
| `StatusBadge` | 成功、警告、错误、信息和中性状态 | 只表达状态，不承载动作 |
| `MaterialCard` / `SettingsSection` / `SettingsField` | 设置页卡片、分组和字段布局 | 设置、偏好等表单型页面 |
| `WorkspaceSearch` | 工具、会话历史和设置的页面搜索；统一搜索图标、焦点、清空与禁用态 | 页面级搜索与筛选，业务状态由页面拥有 |
| `ToolSearch` | 工具结果搜索框 | 工具/技能/MCP 结果页 |
| `MenuItem` / `MaterialSplitButton` | 弹出菜单项和主动作 + 更多菜单 | 上下文菜单、切换菜单和确认动作 |

#### 文字按钮与图标按钮的选择

- **用文字按钮（`MaterialButton`）**表达页面主操作、流程推进、低频操作、含义不直观的操作，以及会改变重要设置或造成损失的操作。按钮文字应直接说明结果，例如“保存更改”“断开连接”“删除会话”；不要要求用户先猜图标含义。
- **用纯图标按钮（`MaterialIconButton`）**表达用户熟悉、低风险且属于次要操作的快捷入口，通常出现在工具栏或重复列表项中，例如关闭、复制、刷新、显隐，以及列表行内的编辑。只有在上下文能明确指出作用对象，且文字会造成明显拥挤或重复时才使用。
- **同一并列操作组保持相同呈现方式和尺寸密度**：一组按钮都用文字，或都用图标；不要把同组的刷新、编辑、删除分别做成图标和文字。危险操作仍保留危险语义样式；若文字操作在紧凑空间放不下，优先让整组换行或收进“更多操作”菜单。
- **删除、清空、重置、移除授权等高影响操作默认使用带文字的菜单项或按钮。** 紧凑列表中若必须提供图标入口，操作对象必须紧邻按钮且清楚可辨；使用危险语义样式，并按该操作的风险提供确认或撤销。确认弹窗中的最终动作必须写明对象和结果，不用垃圾桶图标代替“删除”。
- 同一区域的多个次要操作可以统一为图标按钮；若动作较多、用户不常执行或图标难以区分，将它们收进“更多操作”菜单，并在菜单项中显示文字。不要为了视觉整齐把主操作也改成纯图标。
- 固定、密集的专用控制区可保留约定成熟的图标操作，例如聊天输入区的发送/中断、录音和附件按钮，以及日期翻页、数字步进；它们必须有准确的可访问名称，并跟随当前动作或状态更新。
- 每个纯图标按钮都必须设置准确的 `label` 作为可访问名称；需要视觉提示时可再设置 `title`，但 tooltip 不能代替可访问名称。动作对象动态变化时，名称也要包含对象，例如“编辑 Provider X”。

区块级展开/收起统一使用 `MaterialCollapsible`，不要直接使用原生 `<details>/<summary>` 或另造箭头、按钮和折叠动画。整张资源卡片的展开仍由 `ExpandableContextCard` 管理；JSON 树节点等领域专用结构继续使用对应组件（如 `JsonView`）。

日期选择器的日历格、JSON 树节点、快捷键捕获、数字步进，以及任务/记忆整卡点击属于组件内部的专用交互，仍可使用原生 `button`，但必须沿用 token、焦点态和可访问性语义；它们不应被强行改造成文字按钮。

按钮与表单控件统一使用 `data-width` 尺寸策略：`content`（随内容取宽）、`compact`（使用紧凑控件上限）、`standard`（使用标准设置控件上限）、`fill`（填满父级字段）和 `equal`（在 Flex 操作组中等分）。`MaterialButton`、`MaterialSelect`、`MaterialNumberField`、`MaterialNumberFieldWithUnit`、`MaterialAutocomplete` 与 `ApiKeyField` 通过 `width` prop 指定；普通输入可直接使用同名 `data-width`。`SettingsField` 默认采用 `standard`，可通过 `controlWidth` 选择其他非等分模式。设置字段统一左侧显示标签与说明，右侧对齐输入、开关和操作控件；设置页文字按钮至少采用 `compact` 宽度，成组切换按钮使用 `equal`。父布局负责响应式换行、列数和间距；避免在单个按钮或输入框上写固定像素宽度。

### 2.6 图标原语

所有可复用图标统一从 `lib/Icon.svelte` 调用，图形定义集中在 `lib/icons.ts`。图标使用统一的 `24×24` viewBox、`currentColor` 和默认描边，调用方只指定语义名称与必要的尺寸变体：

```svelte
<Icon name="copy" size={16} />
<Icon name="chevronDown" size={12} strokeWidth={2.5} />
```

禁止在页面或组件中重新内联已经存在的复制、关闭、箭头、文件、状态等图形；新增图形先补入 `icons.ts`，再通过名称调用。品牌标记（`HavenMark`）和 Markdown 生成的静态 HTML 属于明确例外。

---

## 3. CSS 命名与样式规则

### 3.1 命名约定

| 层级 | 命名风格 | 示例 |
|---|---|---|
| 全局组件类 | `md-` 前缀 | `.md-btn`, `.md-card`, `.md-input` |
| 组件内部类 | 连字符分隔 | `.history-item`, `.select-checkbox`, `.card-header` |
| 变体类 | `data-variant` 属性 | `[data-variant='primary']` |
| 状态类 | Svelte `class:` 指令 | `class:selected`, `class:expanded`, `class:open` |
| 子元素类 | 连字符，父级前缀 | `.card-header`, `.card-actions`, `.form-row` |

### 3.2 样式规则

1. **所有颜色值必须使用 CSS 变量**，禁止硬编码
2. **间距使用 `--md-sys-space-*` 变量**，禁止硬编码 px
3. **圆角使用 `--md-sys-shape-*` 变量**
4. **过渡动画使用 `--md-sys-motion-*` 变量**
5. **阴影使用 `--md-sys-elevation-*` 变量**
6. **组件内样式使用 `<style>` 块**，不写全局样式
7. **全局样式只写在 `app.css`** 中
8. **变体样式使用 `data-variant` 属性选择器**，避免多条件 class 判断

### 3.3 状态层模式

所有可交互元素实现 M3 state layer：

```css
.interactive-element {
 position: relative;
 overflow: hidden;
}
.interactive-element::after {
 content: '';
 position: absolute;
 inset: 0;
 background: currentColor;
 opacity: 0;
 transition: opacity var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
 pointer-events: none;
}
.interactive-element:hover::after {
 opacity: var(--md-sys-state-hover-opacity);
}
.interactive-element:focus-visible::after {
 opacity: var(--md-sys-state-focus-opacity);
}
.interactive-element:active::after {
 opacity: var(--md-sys-state-pressed-opacity);
}
```

### 3.4 动画

模态弹窗统一组合 `MaterialDialog` 外壳；遮罩、面板、键盘/背景关闭和双向过渡只在此处实现。`MaterialDialog` 使用 Svelte 内置 fade/scale 过渡，统一为 300ms 淡入淡出及 92% 缩放。权限确认弹窗和日期选择器保留各自内容与业务逻辑，但必须复用该外壳。菜单等非模态浮层使用 CSS keyframes：

```css
@keyframes menuIn {
 from { opacity: 0; transform: translateY(-4px); }
 to { opacity: 1; transform: translateY(0); }
}
```

动态列表项使用全局 `.motion-list-item` 类；整组列表可在容器上使用 `.motion-list`，其直接子项共享淡入效果。会话消息、附件预览和浮层可使用 Svelte transition。全局 CSS 必须遵守 `prefers-reduced-motion: reduce`，工作区脚本触发的 Web Animations 也必须跳过该偏好下的动效。

---

## 4. 状态管理

### 4.1 分层架构

```
component-local ($state / $derived)
    ↑  props / callbacks  ↓
route-level ($state)
    ↑  subscribe  ↓
shared stores (domain-specific writable stores in lib/)
```

Route 页面通过领域 `*Commands.ts` adapter 调用已有 command；只有 `+layout.svelte` 等 App shell 可以直接调用由 shell 独占编排、且已登记 IPC owner 的跨域启动/生命周期 command。

### 4.2 组件本地状态

使用 Svelte 5 runes：

```svelte
<script lang="ts">
 let open = $state(false);
 let count = $state(0);
 let items = $state([]);

 let doubled = $derived(count * 2);
 let display = $derived.by(() => {
  return items.map(i => i.name).join(', ');
 });
</script>
```

### 4.3 跨组件共享状态

跨组件共享状态使用 `svelte/store` `writable`，放在 `lib/` 下对应的领域 store 模块中，不在组件内重复创建：

```js
export const messagesStore = writable([]);
export const notificationStore = writable([]);
```

在组件中订阅：

```js
import { onDestroy } from 'svelte';
import { notificationStore } from '$lib/notificationStore.ts';
import { syncStore } from '$lib/syncStore.ts';

let items = $state([]);
const unsubscribe = syncStore(notificationStore, (v) => (items = v));
onDestroy(unsubscribe);
```

组件直接订阅 store 时必须在组件销毁时调用返回的 unsubscribe；路由容器用 `$effect` 管理订阅时，应返回 unsubscribe 作为 effect cleanup。

### 4.4 $effect 使用场景

- 同步 prop 到本地状态（类似 watch）
- 响应式执行副作用（如自动滚动）
- 监听 URL 变化

**禁止**在 `$effect` 中修改 `$state` 变量（会导致无限循环），除非有明确的条件守卫。

---

## 5. 事件处理

### 5.1 DOM 事件

使用 Svelte 5 事件语法（小写 `on` 前缀）：

```svelte
<button onclick={handler}>Click</button>
<input oninput={handleInput} />
<div onkeydown={handleKeydown}>
```

### 5.2 事件冒泡控制

在可点击卡片或容器中，子操作按钮必须阻止冒泡：

```svelte
<button onclick={(e) => { e.stopPropagation(); onEdit?.(); }}>
 Edit
</button>
```

### 5.3 键盘事件

全局键盘监听使用 `<svelte:window>`：

```svelte
<svelte:window onkeydown={handleKeydown} />
```

### 5.4 回调约定

- 回调 prop 命名：`onChange`, `onClick`, `onClose`, `onToggle`, `onSelect`, `onConfirm`
- 调用时使用可选链：`onConfirm?.(result)`
- 回调参数尽量简洁：`onChange(v)` 而非 `onChange({ value: v })`

聊天 Markdown 中的 HTTP(S) 链接和绝对本地路径渲染为统一可交互链接。单击或键盘激活时通过 `open_external` 打开，右键复制目标；渲染出的锚点不直接暴露外部 `href`，打开请求仍由后端安全策略校验。

---

## 6. Tauri 桥接

### 6.1 调用规则

页面通过领域 command adapter 调用 Tauri；adapter 内部经 `$lib/tauri.ts` 的 `invoke`，禁止页面直接调用 `invoke` 或使用 `@tauri-apps/api`：

```ts
import { loadItems } from '$lib/itemsCommands.ts';
import { reportError } from '$lib/errorHandling.ts';

async function loadData() {
 try {
  return await loadItems();
 } catch (error) {
  reportError(error, { context: 'itemsView', message: '加载内容失败' });
  return [];
 }
}
```

### 6.2 事件监听

在 `onMount` 中开始注册，并同步返回卸载清理函数；若组件在异步注册完成前卸载，注册完成时立即调用 unlisten：

```ts
import { onMount } from 'svelte';
import { registerListeners } from '$lib/events.ts';

onMount(() => {
	const listeners = registerListeners(
		{
			'event:name': (event) => {
				// 处理事件
			},
		},
		{ tag: 'itemsView' },
	);
	return () => listeners.dispose();
});
```

注册失败由 `events.ts` 记录并吞掉，不在页面重复上报；返回的 `dispose` 同时处理异步注册与卸载竞态。

### 6.3 错误处理

- 领域 command adapter 必须把调用失败映射为错误状态或 `reportError` 通知
- 浏览器环境中的命令调用会拒绝；预期不可用的能力应显式显示不可用状态，不得把失败当成成功或静默吞掉
- 用户可见错误使用 `addNotification` 或 UI 提示

---

## 7. 可访问性

| 模式 | 要求 |
|---|---|
| 可交互元素 | `role="button"` + `tabindex="0"` + `onkeydown` 处理 Enter/Space |
| 图标按钮 | `aria-label` 描述操作 |
| 复选框 | 隐式 `<label>` 包裹或 `aria-label`/`aria-labelledby` |
| 弹窗 | `role="dialog"` + `aria-modal="true"` |
| 状态区域 | `role="status"`；普通进度用 `aria-live="polite"`，需要立即处理的错误用 `aria-live="assertive"` |
| 下拉选择 | 具名 `role="combobox"` 控制 `role="listbox"`，以 `aria-activedescendant` 表达活动选项；方向键与 Home/End 导航，Enter/Space 选择，Escape 关闭 |
| 展开控件 | `aria-expanded` + `aria-haspopup` |

---

## 8. 文件与目录结构

下图列出路由、壳层和常用模块的代表性文件，不是完整文件清单。领域 store、command adapter 和 controller 按职责分置独立模块，不通过跨领域聚合桶转发。

```
ui/src/
├── app.css                 # 全局 token + 组件原始类
├── app.html                # SvelteKit shell
├── lib/
│   ├── AppShell.svelte     # 工作区壳层
│   ├── Icon.svelte          # 统一尺寸与可访问性的图标原语
│   ├── icons.ts             # 唯一图形定义和静态 HTML 图标渲染器
│   ├── tauri.ts            # Tauri 桥接懒加载
│   ├── themeStore.ts       # 主题管理
│   ├── notificationStore.ts # 通知状态 owner
│   ├── toolRunStore.ts     # ToolRun 投影状态 owner
│   ├── settingsCommands.ts # 设置命令适配器
│   ├── toolsCommands.ts    # 工具命令适配器
│   ├── ChatBubble.svelte   # 聊天气泡
│   ├── ConfirmationDialog.svelte
│   ├── Logo.svelte
│   ├── MaterialBadge.svelte
│   ├── MaterialButton.svelte
│   ├── MaterialCard.svelte
│   ├── MaterialDialog.svelte
│   ├── MaterialIconButton.svelte
│   ├── MaterialCollapsible.svelte
│   ├── MaterialNumberField.svelte
│   ├── MaterialSelect.svelte
│   ├── MaterialSwitch.svelte
│   ├── McpEditDialog.svelte
│   ├── McpServerCard.svelte
│   ├── NotificationToast.svelte
│   ├── RecordingIndicator.svelte
│   ├── SkillCard.svelte
│   ├── SkillDetailDrawer.svelte
│   ├── SessionMessageTimeline.svelte
│   ├── SettingsSection.svelte
│   ├── ToolRunTimelineCard.svelte
│   ├── ToolResultCard.svelte
│   └── views/
│       ├── SettingsView.svelte
│       ├── ToolsView.svelte
│       └── MemoryView.svelte
└── routes/
    ├── +layout.svelte      # 布局 + 事件总线
    └── +page.svelte        # 聊天页与工作区 Tab
```

### 8.1 Material 组件命名规则

`lib/` 中的 Material 组件遵循以下命名：

| 组件 | 文件名 | 类名 | 变体属性 |
|---|---|---|---|
| 按钮 | `MaterialButton.svelte` | `.md-btn` | `--filled`, `--outlined` 等 class 修饰 |
| 卡片 | `MaterialCard.svelte` | `.md-card` | `data-variant` |
| 徽章 | `MaterialBadge.svelte` | `.md-badge` | `data-variant` |
| 图标按钮 | `MaterialIconButton.svelte` | `.md-icon-btn` | `data-variant` |
| 工具栏 | — | `.md-toolbar` | 统一对齐与间距 |
| 对话框 | `MaterialDialog.svelte` | `.md-dialog` | — |
| 切换开关 | `MaterialSwitch.svelte` | `.md-switch-track` | `:checked` 伪类 |
| 数字输入 | `MaterialNumberField.svelte` | `.md-number-field` | — |
| 下拉菜单 | `MaterialSelect.svelte` | `.md-select-container` | — |

---

## 9. Svelte 5 语法规则

### 9.1 强制规则

| 语法 | 允许 | 禁止 |
|---|---|---|
| Props | `$props()` 解构 | `export let` |
| 响应式状态 | `$state()`, `$derived()`, `$derived.by()` | Svelte 4 `$:` 标签 |
| 副作用 | `$effect()` | Svelte 4 `$:` 响应式赋值 |
| 内容分发 | `{@render children?.()}`, `{#snippet}` | Svelte 4 `<slot>` |
| 双向绑定 | `bind:value` 用于表单元素 | 组件间 `bind:prop` |

### 9.2 推荐模式

- `$derived` 用于简单派生值
- `$derived.by()` 用于需要多条语句的派生
- `$state` 初始化空数组：`$state([])`，空对象：`$state({})`
- Set 类型响应式：通过创建新实例触发更新 `selectedIds = new Set(next)`

### 9.3 与 Svelte 4 store 的桥接

当需要从 `svelte/store` 的 `writable` 读取数据时：

```js
import { onDestroy } from 'svelte';
import { notificationStore } from '$lib/notificationStore.ts';
import { syncStore } from '$lib/syncStore.ts';

let items = $state([]);
const unsubscribe = syncStore(notificationStore, (v) => (items = v));
onDestroy(unsubscribe);
```

不要在 Svelte 5 组件中创建新的 `writable` store，使用 `$state` 替代。

---

## 10. UX 信息架构与交互契约

> 本节定义当前工作区的信息架构、交互契约和响应式布局。具体实现边界以现有组件与 ADR 为准。

### 10.1 设计目标与不变边界

Haven 的核心场景是“快速开始一段对话，并清楚知道它当前在做什么”。界面应围绕这个场景组织，而不是让用户在页面、状态菜单、工具卡片和设置表单之间寻找当前上下文。

目标原则：

1. **对话优先**：打开应用后，用户首先看到当前会话、输入区和下一步操作；其它能力按需展开。
2. **上下文连续**：会话、工具调用、确认、后台任务、定时任务和错误都要在同一条可追溯的上下文中呈现。
3. **状态可行动**：每个“未配置、失败、等待、断开、保存中”状态都必须说明原因，并提供明确的下一步动作。
4. **渐进披露**：默认只展示完成当前任务所需的信息，详情、调试信息和低频操作放入抽屉、详情面板或二级区域。
5. **统一而有层级**：同一种功能只保留一种组件和一套视觉契约；层级通过尺寸、间距、颜色和位置表达，不通过随意混用圆角、直角和高度表达。
6. **桌面优先，窄窗可用**：以桌面窗口为主设计，同时保证 1440px、1024px、800px 和约 455px 宽度下不溢出、不遮挡主要操作。
7. **可恢复**：停止、结束、重试、撤销、放弃更改等动作的语义必须区分清楚，且在风险操作前给出必要确认。
8. **契约稳定**：优先在前端重组信息架构和交互，不随意修改 Rust 命令、数据库字段、事件名、ID 语义或持久化协议。确有必要时必须先记录影响和迁移方案。

明确边界：

- UI 改动可以重排页面、拆分编排和提取公共组件，但不能为了视觉效果破坏现有后端行为。
- 所有共享 token、基础控件和状态表现必须有唯一实现；禁止新旧两套按钮、Tab、状态徽章或保存栏长期并存。
- `lib/` 中的领域 view 可拥有本领域的加载状态和编辑草稿，但通过 `*Commands.ts` 调用 Tauri；可复用组件经 props/callback 工作。路由与 App shell 保留跨域装配和应用生命周期职责，遵守本文第 2.4 节。

### 10.2 信息架构

当前工作区采用“工作区壳层 + 对话主工作区 + 按需上下文”的结构：

```text
Haven 工作区
├── 工作区导航
│   ├── 对话（默认入口）
│   ├── 工具
│   ├── 历史
│   │   ├── 会话历史
│   │   ├── 任务历史（后台任务 / 定时任务）
│   │   └── 记忆
│   └── 设置
├── 对话工作区
│   ├── 会话栏：新建、切换、重命名、结束
│   ├── 会话头部：标题、模型、当前会话状态、会话操作
│   ├── 消息时间线：消息、思考、工具调用、确认和结果
│   └── 统一输入区：文字、语音、图片/文件、发送、停止
└── 上下文层（按需打开）
    ├── 任务详情抽屉
    ├── 工具/技能详情抽屉
    ├── 会话信息和调试详情
    └── 全局通知中心
```

信息架构规则：

- “对话”是默认首页；用户不需要先进入状态菜单才能知道当前会话是否运行。
- “历史”统一承载可回顾内容，并在内部按“会话历史 / 任务历史 / 长期记忆”分为二级页签。全局导航另提供“任务”和“长期记忆”直达入口，分别打开对应分区；活动任务状态入口也复用任务路由。会话与任务卡片按生命周期统一分为“进行中”和“已结束”两组；任务历史覆盖后台任务和定时任务，状态徽标继续区分完成、失败和取消。
- 对话中的工具调用、确认、后台任务和定时任务必须保留返回来源；用户从任务打开时，应能回到触发它的会话。
- 工具页采用列表/详情布局：左侧或上方是可搜索、可筛选列表，右侧或抽屉是详情与操作；不得为每个项目复制一套卡片操作逻辑。
- “历史”只负责可回顾的信息：会话历史可以点击卡片打开并继续，任务历史展示后台任务和定时任务的当前状态与最近执行结果，长期记忆页通过统一的记忆中心浏览、管理和检索事实/过去的对话；三者共享历史上下文，但不互相复制列表。会话侧栏只列出可继续的会话，并明确提示已完成记录前往“历史”查看。
- 设置页按用户目标分组，而不是按实现模块堆叠：对话与行为、模型与连接、语音与媒体、界面与通知、安全与权限、性能与限制、日志与诊断。顶部搜索可跨分类定位字段，并在选中结果后切换分类、滚动到对应控件。所有分类共用 `--md-sys-content-max-width` 内容宽度上限；表单统一采用左侧标签/说明、右侧控件/动作的两列布局，在设置内容容器宽度 640 CSS px 及以下切为单列。分组卡片默认纵向铺满内容区；选项网格按控件最小可读宽度自适应列数，不另设分类专属断点。
- 一级导航、页面内分组和局部筛选不是同一种 Tab。只有在同一上下文内切换同层内容时才使用 Tab；否则使用导航项、分段控制或筛选器。
- 页面内二级分类导航在窄布局下共用同一水平样式：标签内容能放下时均匀填充整行；放不下时保留标签自然宽度并允许横向滚动，避免压缩或截断名称。隐藏原生滚动条，并按滚动位置渐隐左右边缘提示仍有内容；支持鼠标拖动和纵向滚轮转横向滚动，交互手感参照 Markdown 表格/代码块；达到 840 CSS px 后统一切换为左侧分类列表。
- 对话布局同时依据宽度、方向和可用高度调整：竖屏窄窗将会话切换、生命周期操作和输入工具分组排列；短横屏压缩顶部与输入栏高度，保留完整操作并扩大消息阅读区；达到 1200 CSS px 时保留左侧会话栏，确保最大 360px 侧栏下聊天区仍有约 688px；标题、消息、欢迎页与输入工具按 `conversation` 容器实际宽度调整，不以整个窗口宽度代替聊天区宽度。
- 工具和历史布局按内容面板宽度排列筛选项与操作；窄屏筛选分组呈现，列表保持单列可读，短横屏收紧表面留白和分组间距，桌面端保留分类导航与完整详情。
- 设置分类沿用同一套窄屏横向导航与宽屏侧栏；短横屏收紧表面、标题和分类间距。统一保存操作固定在页面标题右侧，采用中性灰按钮，无修改时禁用，有草稿时显示“保存更改”；操作区显示修改分类、放弃动作和持久保存错误。

### 10.3 交互和状态契约

所有需要异步加载或提交的领域状态，都必须能映射到以下状态集合：

```text
idle → loading → ready | empty | unconfigured | error
ready + 用户修改 → dirty → saving → saved | error
```

统一规则：

- `loading`：显示稳定的占位结构，不通过页面整体闪烁制造等待感。
- `empty`：说明为什么为空，并给出创建、搜索或导入动作；不能只显示一张空白卡片。
- `unconfigured`：解释缺少哪项配置，并提供“去配置/立即配置”动作；不能只显示灰色徽章。
- `error`：说明影响范围，提供重试；需要用户修复配置时，提供跳转到对应设置组的动作。
- `dirty`：在页面标题或分组动作栏附近显示未保存提示；离开页面或关闭窗口时按风险提供保护；分类切换保留草稿，不触发离开确认。
- `saving` / `saved`：保存动作有进行中、成功和失败三种反馈；成功提示应短暂且不遮挡主内容，失败信息应保留在相关分组旁。
- `stopped` 与 `ended` 必须区分：停止是终止当前运行，允许继续同一会话；结束是关闭会话并退出当前工作上下文。
- `retry` 只针对可重试的失败；不可重试错误必须告诉用户需要改变什么。
- Agent 执行摘要只描述可验证的状态和调用数量；已知失败、取消、结果未确认和权限等待必须显眼说明，并给出允许/拒绝等下一步动作。
- 语音输入持续显示录音或转写状态；录音与转写均可通过可见按钮或 Esc 取消，转写结果沿用普通消息提交路径并保留录音关联。

设置页的分类是“对话与行为、模型与连接、语音与媒体、界面与通知、安全与权限、性能与限制、日志与诊断”。同一份父级草稿跨分类保留；在设置页内切换分类不弹离开确认。后端 `update_settings` 接收一份完整的 `Settings` 快照，设置页因此只提供统一的“保存 / 放弃”，并在页面标题操作区和每个有修改的分类列出范围；保存按钮始终位于标题右侧，无待保存修改时禁用，有修改时启用。主题和强调色由前端本地保存并立即生效，不计入后端配置草稿；自动启动仍通过专用命令应用。

配置保存采用 durable-first apply：配置写入后，后续运行时阶段若失败，界面说明“已写入配置但部分运行时未应用”，不自动重试；应用重启后从磁盘配置重新初始化。详见 ADR 0351 与 ADR 0372。

状态展示优先级：

1. 当前会话的详细执行状态显示在会话头部和相关消息/工具卡片附近；全局顶栏可以用短文案概括 ReAct Loop 阶段。
2. 后台任务或定时任务显示在任务中心。后台 shell 任务在会话中并入发起它的工具调用卡，继续使用该卡显示参数、实时输出、终态与等待提示；缺少来源工具消息时使用相同工具卡作为恢复兜底。工具思考或任务仍在运行时置于会话时间线末尾以突出进度；终态后台任务按来源步骤归位，输出仍显示在发起它的工具卡中。定时任务保留自己的来源卡。顶栏任务入口只作为快捷入口和数量摘要。
3. 全局顶栏将 Loop 执行状态和模型配置/连通状态合并为一个状态 chip，按以下优先级只显示当前最需要注意的一项：浏览器预览 → 录音/转写 → 会话错误 → 模型不可用 → 会话暂停或等待操作/任务 → 模型未配置 → Loop 执行阶段 → 模型检测中 → 加载中/空闲。正常连通时不显示常驻成功文案；被覆盖的执行、模型和任务信息保留在状态 chip 的悬停说明中。
4. 状态圆点使用统一语义色：红色表示录音中、执行错误或模型不可用；蓝色表示转写/生成；琥珀色表示暂停、等待操作、未配置、检测中及排队/请求/响应/运行/加载；紫色表示等待工具结果或后台/定时任务；灰色表示空闲或浏览器预览。录音、转写、生成、请求/响应等待、工具结果等待、排队/运行、后台任务运行、加载和模型检测使用呼吸动画；错误、未配置、暂停、用户/任务等待和空闲保持静止。
5. 通知 toast 只用于短暂结果（如“已保存”“已复制”）；持久错误、未配置和需要操作的状态必须留在上下文中。

交互几何契约：

- 基础按钮、输入框、Tab、徽章和卡片使用第 1 节的唯一 token；禁止在局部重新发明圆角、内边距、控件高度或下划线。
- 同一行的按钮和输入控件使用同一控件高度；主要内容区、标题栏、状态栏和底部动作栏按同一垂直节奏对齐，不能出现“顶部过窄、底部过高”的视觉失衡。
- 主按钮只保留一个明确的视觉重点；删除、清空、撤销等危险动作使用红色 danger 变体，列表中的轻量删除使用 danger-text；完成类动作使用绿色 success，待处理或需关注入口使用黄色 warning。颜色辅助识别，按钮文案仍须说明动作含义。
- 短图标动作使用统一方形命中区域和 `aria-label`；带完整动作语句的 CTA 才使用宽按钮。
- Tab 激活态由 `MaterialTabs` 与工作区导航共享选中容器、文字色和蓝色滑动指示条；水平布局将指示条放在底部，侧栏布局放在左侧。指示条随选中项平滑移动；颜色、尺寸和间距统一使用 token，页面不得另造局部选中态。
- 所有持久选中语义（`.selected`、`aria-selected`、`aria-checked`、`aria-pressed`、`aria-current="page"`）统一使用 `app.css` 中的 `--md-sys-selection-outline-*` token；主色填充的选项使用 `on-primary` 保证边缘对比。控件键盘焦点统一使用 `--md-sys-focus-ring`，文本输入焦点边框使用主色；各组件原有的选中底色和 Tab 指示器继续表达各自语义。

### 10.4 核心用户流程

实现时优先保证以下流程在真实数据、空数据、错误和窄窗状态下都成立：

| 流程 | 必须清楚的内容 | 完成标准 |
|---|---|---|
| 新建 / 切换会话 | 当前会话、会话列表、正在运行的会话、创建入口 | 新建后自动聚焦输入区；切换不会丢失未提交文本；运行状态可见 |
| 发送消息 | 输入方式、附件、发送和停止 | 发送后立即出现用户消息；生成中可停止；停止后可继续；失败可重试 |
| 语音输入 | 录音/转写状态、取消方式、转写结果去向 | 状态始终可见；录音或转写可点击取消或按 Esc 取消；结果作为语音消息进入当前会话或新会话 |
| 工具调用与确认 | 工具做什么、影响范围、允许/拒绝 | 确认项出现在消息上下文；允许、拒绝和始终允许语义不同；同步结果与后台 shell 进度共用工具调用卡并可展开查看 |
| 后台任务 / 定时任务 | 来源、进度、状态、下一步 | 后台 shell 卡锚定其来源工具步骤；能从任务中心回到来源会话；取消后有明确结果。定时任务保留独立来源卡 |
| 切换 Chat 模型 | 当前配置、切换结果 | 输入区模型入口可切换已配置模型；切换后所有会话的后续对话请求使用该配置；思考与联网选项按需展开 |
| 配置模型 / provider | 当前配置、验证结果、缺少字段、代理路由、wire protocol 与媒体 endpoint | Provider 预设分类表达服务身份；`api_style` 单独表达线协议，保留厂商 adapter 特调身份。STT/TTS/图片生成按服务端实际支持的 endpoint 单独启用；Provider 可选默认环境代理、直连或指定 HTTP(S) 代理及绕过主机；失败定位到具体字段或连接 |
| 保存设置 | 修改范围、保存进度、失败原因 | 分组脏状态可见；保存成功不丢焦点；离开时能保存、放弃或返回编辑 |
| 结束会话 | 结束与停止的差异、潜在影响 | 结束前给出确认；结束后回到明确的会话列表或新建状态，不留下“看似可输入但实际已结束”的界面 |

### 10.5 组件和状态所有权

重构过程中优先建立以下可复用组件/编排边界。名称可以按现有项目命名规范调整，但职责不能合并回一个巨型页面：

| 目标边界 | 负责什么 | 不负责什么 |
|---|---|---|
| `AppShell` / 工作区壳层 | 顶栏、工作区导航、全局布局、响应式断点 | 会话业务、工具加载、设置保存 |
| `WorkspaceNav` | 一级入口、当前工作区、窄窗折叠 | 具体页面数据请求 |
| `WorkspaceSurface` | 次级工作区的统一主题背景和入场动效 | 页面数据、筛选器、列表和详情内容 |
| `SessionRail` | 新建、搜索、切换、会话级操作 | 生成循环、消息持久化 |
| `SessionHeader` | 标题、执行工作区入口、模型、会话状态、停止/结束等会话动作 | 任务列表和全局通知 |
| `SessionMessageTimeline` | 消息、思考、工具调用、确认、结果的展示编排 | 领域命令调用 |
| `Composer` | 文字、语音、附件、发送/停止交互 | 决定后端任务状态 |
| `ToolRunCenter` / `ToolRunTimelineCard` | 会话、后台任务、定时任务的统一展示和操作 | 修改消息时间线内部数据 |
| `AsyncState` / `EmptyState` / `ErrorState` | 统一加载、空、未配置、错误反馈 | 领域数据获取 |
| `SettingsNav` / `SettingsSection` / `SettingsActionBar` | 设置分组、分组校验、保存/放弃动作 | 直接读取其它页面业务状态 |
| `ResourceList` / `ResourceDetail` | 工具、技能、MCP 等资源的列表/详情模式 | 为每种资源复制一套视觉系统 |

对话标题区的执行工作区下拉框提供自动检测、当前目录和已保存目录；选择新目录后复用安全设置更新流程立即应用并持久保存，且加入本机快捷列表。聊天标题区和安全设置中的目录选择都会把成功选中的路径加入该列表；列表按最近使用顺序最多保留 20 项，删除按钮只移除快捷项，不删除目录或更改当前执行工作区。列表保存在前端本机存储，不进入 `config.toml`。

状态所有权必须沿着“领域编排 → 容器组件 → 纯展示组件”单向流动。一个状态只能有一个权威来源；组件不得通过复制 prop、重复订阅或页面级 CSS 覆盖制造第二份状态。提取组件前先用 `rg` 搜索相同结构、类名、token 和事件处理，确认是否应该复用现有实现。

### 10.6 横版工作台与窄窗布局

当前桌面横版采用独立的工作台编排：左侧是一级工作区导航，左下方放置紧凑的全局状态和主题切换；一级导航统一采用 152px 的紧凑文字侧栏：顶部保留品牌图标与名称，导航项以图标加完整名称单行排列，辅助说明通过悬停标题提供，不再提供展开/收起切换。底部第一行显示完整状态文字，第二行右侧放主题切换，有任务活动时左侧显示任务入口；低高度窗口中导航列表独立滚动，品牌与底部控件保持可达。工作区名称由左侧导航表达，避免再次显示“对话”“历史”等重复标签；600 CSS px 及以上横屏不额外显示空白网页标题栏，拖动和窗口控制由系统标题栏承接。进入对话后，工作区再分为会话列表与对话画布；会话标题区集中当前会话标题、token 用量和新建/完成动作。消息列可随桌面空间放宽，但仍保留可读的最大行宽。工具、历史、记忆和设置使用完整横向画布；宽屏下历史、工具和设置采用左侧分类、右侧内容的工作区布局，三个工作区共用相同的侧栏宽度、分隔线和 `MaterialTabs` 视觉样式，表单字段和正文继续使用适合阅读的宽度。窄窗无侧栏时，状态和主题切换留在顶栏。

主布局按 WebView 可用宽度切换，并参考 Material 3 自适应窗口尺寸级别：低于 600 CSS px 使用紧凑的纵向壳层和聊天页内会话入口；从 600 CSS px 起显示固定 152px 图标加完整名称导航；达到 840 CSS px 后，聊天页显示常驻会话栏，工具、历史、记忆和设置页切换为左侧分类导航；主导航仅保留这一套样式，不提供宽窄切换，也不在 1200 CSS px 自动展开。宽高比不再单独决定多栏布局，避免窄窗口仅因比例较宽就同时挤入多个侧栏；窗口高度和方向继续影响局部排布与滚动。600 CSS px 及以上的横屏隐藏没有操作内容的网页标题栏并让主内容顶上，竖屏保留标题栏并使用与内容区相同的 surface 背景。超宽端仍将工作台内容限制在 21:9 并居中。应用默认以 1280×720 横屏启动，CSS px 是本项目的视口断点单位，600/840 是结合窗口尺寸参考和 Haven 面板宽度选定的布局阈值。导航项保留轻微的选中态过渡，会话栏拖动直接跟随指针，侧栏入场只做短距离淡入。断点只选择展示布局，不创建另一份业务状态或路由：所有尺寸共享 `/` 与 `?tab=` / `?section=`、会话 reducer、事件 owner 和操作回调。

桌面主窗口默认以 1280×720 启动，窗口可缩放范围为 2:3 至 21:9；拖动缩放或最大化时，Windows 会把窗口约束在此范围内，避免进入极端比例导致布局无法正常显示。静态尺寸矩阵用于审查响应式布局。

桌面会话栏默认宽 240px，可折叠到 48px；折叠态保留展开与新建入口。右边缘支持指针拖动及方向键以 16px 步长调整，范围为 200–360px，Home/End 调至边界，双击恢复 240px；重新展开保留本次调整宽度并将焦点移至对应切换按钮。主导航没有展开状态；会话栏折叠状态与宽度由前端 `sessionSidebarLayoutStore` 持有，跨工作区切换保留，应用重启后恢复默认，不写配置或本机存储。`SessionSidebar` 只拥有布局交互，`SessionRail` 只消费聊天路由已有的会话列表、新建与切换回调，不负责加载或持久化；低于 1200 CSS px 时，会话侧栏收起，会话切换与新建入口放在会话标题区，输入区只保留模型相关控件。对话切换列表包含运行时非终态会话和历史中的暂停/错误会话，完成会话只从“历史”工作区打开；重新打开完成会话后，其持久状态改为暂停。响应式布局不得复制 transcript、ask/input 决策、滚动状态或 Tauri 事件订阅。两种布局必须继续保留发送、停止、结束、重试和工具确认的原有语义，且窄视口下主操作、未配置提示及持久错误仍可到达。

本布局的静态审查尺寸为 2560×1080、1920×1080、1440×900、1280×720、1024×768、800×600、768×768 和 480×720，并在 599/600、839/840 CSS px 附近检查断点两侧，并在 1199/1200 CSS px 检查会话侧栏显示切换（主导航不变）：2560×1080 验证 21:9 内容上限；1920×1080、1440×900 与 1280×720 验证统一文字主导航与完整桌面布局；1024×768 验证统一文字主导航与单列聊天区；800×600 和 768×768 验证 600–839 CSS px 的图标加完整名称导航与单内容栏；480×720 验证低于 600 CSS px 的窄版壳层。同时检查 320×720、390×844、960×480、1280×720 以 150% / 200% 缩放后的等效 CSS 视口，确认标题操作可换行、输入区可到达、页面无横向溢出。竖版手机界面的后续深化可以调整导航为抽屉或底部入口、将详情改为 sheet，但不得更改本节列出的状态与 IPC 契约。

---

## 附录 A: 常见反模式

| 反模式 | 正确做法 |
|---|---|
| 硬编码颜色 `color: #3378D6` | 使用 `var(--md-sys-color-primary)` |
| 硬编码间距 `padding: 16px` | 使用 `var(--md-sys-space-lg)` |
| 使用 `<slot>` | 使用 `{@render children?.()}` |
| 使用 `export let` | 使用 `$props()` 解构 |
| 使用 `$:` 响应式标签 | 使用 `$derived` 或 `$effect` |
| `lib/views/` 或可复用组件直接调用 `invoke` | 领域 view 调用 `*Commands.ts`；子组件通过 owner callback 请求动作 |
| 在 `$effect` 中修改 `$state` 变量 | 使用事件处理器或 `$derived` |
| 重复的 CSS 代码（如多个组件实现切换开关） | 提取为公共组件 |

## 附录 B: 快速参考

### 创建新组件

```svelte
<script lang="ts">
 /** @prop {string} title — 标题 */
 let { title = '', children } = $props();
</script>

<div class="component">
 {@render children?.()}
</div>

<style>
 .component {
  border-radius: var(--md-sys-shape-medium);
  padding: var(--md-sys-space-lg);
  color: var(--md-sys-color-on-surface);
 }
</style>
```

### 创建新页面

```svelte
<script lang="ts">
 import { onMount } from 'svelte';
 import { loadItems } from '$lib/itemsCommands.ts';
 import { reportError } from '$lib/errorHandling.ts';

 let data = $state([]);

 onMount(async () => {
  try {
   data = await loadItems();
  } catch (error) {
   reportError(error, { context: 'itemsView', message: '加载内容失败' });
  }
 });
</script>
```

## 运行中输入的投递方式

运行中或正在生成的聊天输入框通过紧凑的双选按钮组提供「安全边界调整」与「完成后追加」（ADR 1156），并将选择的 `deliveryMode` 随文本与附件提交。安全边界调整会等当前模型请求或工具结束后接入，不会中断正在执行的调用；完成后追加等待模型自然完成当前工作后再开始处理。工具续轮、确认/Ask 等待与步骤预算暂停不表示完成。排队、生成、工具执行、交互等待、暂停和终态的投递提示统一放在全局状态 chip 的悬停说明中，输入区不额外显示提示段落；显式选择投递方式的消息会显示提交中、待安全边界/本轮完成后，送达后只保留收到勾号。「待操作」菜单与投递方式控件复用工具栏左侧同一位置：待操作项出现时隐藏投递方式控件，避免并列；待处理交互期间普通发送不可用。未送达的显式投递消息会禁用投递方式控件和普通发送。当前 Ask 答案仍可提交；Ask 等待时不展示投递方式控件，输入文本和已选答案选项均路由为当前问题的回答，不提供完成后追加路径。权限确认待处理期间普通输入不发送，需先处理确认。两个按钮支持键盘 Tab 聚焦以及 Enter/Space 选择。语音继续使用后端 Auto 路由。
