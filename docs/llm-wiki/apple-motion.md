# Apple Design 合同（桌面工作台）

技能来源：[emilkowalski/skills · apple-design](https://github.com/emilkowalski/skills/tree/main/skills/apple-design)。  
计划：[`docs/plans/2026-09-06-apple-design-lightweight-stability.md`](../plans/2026-09-06-apple-design-lightweight-stability.md)。

这是**运动物理 + 一层材质 + 克制**的强制合同，不是 iOS 皮肤。改手势、毛玻璃、弹簧、按下反馈前先读本文。产品面仍走 [dialogs.md](./dialogs.md) / [i18n.md](./i18n.md) / [appearance-skins.md](./appearance-skins.md)。

## 一句话

桌面指挥台：临界阻尼、无 bounce、每窗一层材质、系统字体。手机镜像才允许 sheet / 甩动过冲。任何让 Host 更重或让 WebView 更糊的「苹果味」直接否决。

## 平台

| 面 | macOS | Windows / Linux | 手机镜像 |
|----|-------|-----------------|----------|
| 材质 | 系统 Sidebar vibrancy **或** CSS blur，二选一 | 实心 `--bg-sidebar-solid`，禁止 64px blur | toolbar 一层 frost |
| 运动 | 分栏弹簧 damping **1.0** | 同一套物理；只动 `transform` / `opacity` | sheet 甩动允许 damping **~0.8** |
| 字体 | `-apple-system`，不嵌 SF Pro | Segoe UI，不硬嵌 SF | 系统 UI，间距 `rem` |
| 模态 | `GlassModal`，锚在触发源 | 同左 | 才允许 Vaul 类 sheet |

## 弹簧（仅四条手势面）

实现：`src/lib/motionSpring.ts`（自研，不加 `motion` / `framer-motion`）。分栏**拖拽**范围内 1:1，越过 max 橡胶边，松手硬 clamp。开合仍走现有 CSS pane 插值（WKWebView）；`prefers-reduced-motion` 不关掉分栏宽度（硬切更差）。

只给这些面用弹簧数学：

1. 左栏宽度  
2. 右栏宽度  
3. 底栏终端高度  
4. 手机镜像 sheet  

其余颜色 / hover / 透明度走 CSS。`--motion-pane` 仍是无过冲三次贝塞尔，直到该面迁弹簧。

| 场景 | damping | response |
|------|---------|----------|
| 桌面分栏 / 默认 UI | `1.0` | `0.35–0.40` |
| 手机 sheet 甩动 | `0.8` | `0.30` |
| 菜单、设置、思考块 | 禁止弹簧 | — |

落点用指数衰减投影，不是 `v²/2a`：

```js
project(v, d = 0.998) => (v / 1000) * d / (1 - d)
rubberband(overshoot, dimension, c = 0.55) =>
  (overshoot * dimension * c) / (dimension + c * Math.abs(overshoot))
```

动画必须可打断：从屏幕**当前** transform 起步，交接松开速度。禁止手势驱动的 CSS `width`/`height` transition（中途改目标会跳）。

## 按下反馈

pointer-down 立刻反馈，不要等 `click`。

- token：`--press-scale: 0.88` · `--press-ms: 80ms`
- 覆盖：`.btn` · `.chip` · `.perm-bar__btn` · `.icon-btn` · `.session-row`
- `prefers-reduced-motion: reduce` 时不做 scale

## 材质

- 每窗口运行时 **≤ 1 层** `backdrop-filter`。禁止壁纸 + 系统 vibrancy + 侧栏 CSS blur + 模态 glass 同时开。
- 聊天主柱必须实心（`--bg-main`）。字不写在半透明前景上。
- 进场只动 opacity + scale。**禁止每帧改 `--glass-blur`**（WKWebView GPU）。
- `--glass-blur` / `--sidebar-blur` 固定值。
- `prefers-reduced-transparency: reduce` → 玻璃走 `--glass-surface-solid`，blur = 0，侧栏走 `--bg-sidebar-solid`。
- `prefers-contrast: more` → 近实心底 + `--border-strong`。

## 字号 tracking（token 先在，全面换用走 B3）

- `--track-display: -0.02em`（大标题）
- `--track-ui: 0.01em`（12–13px 控件）
- 正文接近 `0`
- 不嵌自定义 SF Pro 文件

## Companion（未开启则 Host 不 init）

默认 Core：项目 / 会话 / 流式对话 / 权限 / 预览 / Diff / 模型。

默认关：桌宠、壁纸视频、皮肤市场、Office 预览、xterm 底栏、内嵌浏览器、Remote IM、语音。

Remote IM：持久化 `enabled: false` 时**不要**因为磁盘上还有凭据就自动拉起 bridge / watchdog。用户点 Start 再启动，并那时才挂健康检查。

## 禁止

- 全站 Framer Motion / 桌面 Vaul sheet / 再加一层毛玻璃 / 嵌 SF Pro
- 为菜单或设置页加 bounce
- 迁 Electron 或把 grok CLI 打进安装包
- 新增 `src/styles/*.partN.css` 来堆运动

## 相关源码

- token：`src/styles/tokens.css`（`--press-*` · `--track-*` · reduced-transparency）
- IM 启动：`src-tauri/src/remote_im/mod.rs` · `bridge.rs` · `lib.rs` setup
- 桌宠：`src-tauri/src/pet_window.rs`（默认 `enabled: false`，仅 `show_pet` 建窗）
- 镜像：`src-tauri/src/mirror/mod.rs` `maybe_autostart`（仅 `GROK_MIRROR_HEADLESS=1`）
- 语音：`src-tauri/src/voice_host.rs`（`voice_start` 才连网）
