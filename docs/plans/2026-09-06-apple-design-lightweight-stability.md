# 计划书 · Apple Design 薄 UI × Codex 进程模型

| 字段 | 值 |
|------|-----|
| 日期 | 2026-09-06 |
| 基线 | Grok App **v0.2.30**（本仓库实测） |
| 状态 | **本轮交付** · A0/A1/A2 报告/B0/B1/B2 橡胶边/B3 材质 token 已落地。未做：IM sidecar、独立浏览器进程、Vaul 手机 sheet、App.js &lt; 800KB |
| 技能 | [emilkowalski/skills · apple-design](https://github.com/emilkowalski/skills/tree/main/skills/apple-design)（WWDC 2018 Fluid Interfaces + 八原则） |
| 对照 | Codex（Rust CLI + 薄 UI）· Cursor（VS Code / Electron 分叉） |
| 交互稿 | 聊天旁 canvas：`apple-design-frontend-plan.canvas.tsx` · `stack-stability-codex-cursor.canvas.tsx` |

---

## 0. 一页结论（先看这个）

Grok App **壳已经轻，面太宽，手感是 CSS 演出来的，崩溃半径是单进程 Host。**

| 要做 | 不要做 |
|------|--------|
| 学 Codex：UI 只负责呈现，Agent / IM / 浏览器出进程 | 学 Cursor 迁 Electron（体积 ×20，换来你们养不起的 Chromium） |
| 学 apple-design 的**运动物理 + 一层材质 + 克制** | 把桌面画成 iPhone、全站 Framer Motion、再叠一层毛玻璃 |
| Core 默认：会话 / 权限 / 预览 / Diff | 默认塞桌宠、壁纸视频、Office、xterm、11 路 IM |
| 四条手势面用弹簧（可打断） | 菜单、设置、思考块弹来弹去 |
| mac 系统 vibrancy **或** CSS blur，二选一 | 壁纸 + vibrancy + 64px 侧栏 blur + 48px 模态同时开 |

八周目标（可压缩，**顺序不要倒**：先减层再加弹簧，先拆主包再加 `motion`）：

```
App.js 2.7 MB → < 800 KB
backdrop-filter 85 处 → ≤ 12，且运行时每窗 ≤ 1 层
默认驻留：全功能 Host → UI + Session（IM 零）
崩溃：last_crash.txt 一行 → minidump + 会话 id
按下反馈：8 条 :active → 按钮 / 行 / chip / 权限条全覆盖
```

---

## 1. 现在这套栈（实测，不是感觉）

### 1.1 分层

```
┌─────────────────────────────────────────────────────────┐
│  UI  React 19 + Vite 6 + Tailwind 4 + 69 份 CSS         │
│  WebView（Tauri 2 / wry）  主包 App.js = 2.7 MB         │
├─────────────────────────────────────────────────────────┤
│  Host  Rust 2021 + Tokio   227 文件 / 145k LOC          │
│  同一进程：ACP · 11 路 IM · 镜像 HTTP/WS · 媒体协议     │
│           · 侧栏浏览器（unstable multiwebview）· 桌宠   │
├─────────────────────────────────────────────────────────┤
│  grok agent stdio   ← 已经隔离，这是最像 Codex 的一层   │
└─────────────────────────────────────────────────────────┘
```

| 层 | 技术 | 规模 | 稳定性含义 |
|----|------|------|------------|
| 壳 | Tauri 2 + wry · `unstable` multiwebview | NSIS **15.4 MB** / 便携 **48 MB** | 比 Electron 轻一个数量级；子窗是 Win 焦点/崩溃源 |
| Host | Rust + Tokio + axum + portable-pty | `session_manager` 618 KB · `remote_im` 569 KB · `acp_client.rs` 271 KB | IM 已从 `remote-bridge` **搬回进程内** |
| UI | React 19 · Vite 6 · SWC · Tailwind 4 | 前端 **498k LOC** · 组件 292 · lib 1052 · i18n 15 语 | 长会话已虚拟化，主包仍过肥 |
| 编辑/终端 | CodeMirror 6 · xterm · TipTap 3 | chunk 768 / 436 / 513 KB | 预览可以，当 IDE 用会拖死 UI 线程 |
| 媒体 | pdfjs + Office 预览 | Office chunk **990 KB** | 部分 lazy；`media://` 曾 SIGABRT 整进程 |
| 材质 | `--glass-blur: 48px` · `--sidebar-blur: 64px` · `window-vibrancy` | CSS **69 文件 / 917 KB** · `backdrop-filter` **85** 处 · `:active` **8** 条 | 玻璃叠玻璃；按下几乎没反馈 |
| 运动 | `--motion-fast 120ms` · `--motion-pane 320ms` 无过冲 | 15 处 `prefers-reduced-motion` | 分栏是 CSS width，中途改目标会跳 |
| 崩溃现场 | `host_runtime` heartbeat + `last_crash.txt` | 无 minidump | 只有异常码/地址 |

已做对、**计划不拆**：ACP 子进程、Session FSM、park/unpark（默认 8 / 闲置 30 min）、stall watchdog、`panic=unwind`、path_scope、系统字体栈、玻璃/实心菜单分流、禁止 `window.confirm`、SegmentedControl、权限 Ask。

### 1.2 和 Codex / Cursor 比什么

打分 0–10，越高越好。安装体积 / 内存按「轻」给分。

| 维度 | Grok App | Codex | Cursor |
|------|:--------:|:-----:|:------:|
| 进程隔离 | 4 | 9 | 8 |
| 崩溃不拖垮 UI | 3 | 9 | 8 |
| 安装体积 | 8 | 9 | 3 |
| 空闲内存 | 6 | 8 | 3 |
| 功能克制 | 3 | 8 | 4 |
| 协议（ACP） | 8 | 9 | 5 |
| 崩溃现场 | 5 | 7 | 8 |
| 插件模型 | 6 | 8 | 9 |

- **Codex**：CLI / app-server 才是产品。VS Code 扩展和 TUI 是薄客户端。Agent 崩了编辑器还在。
- **Cursor**：VS Code 分叉。稳靠 Chromium 多进程（Main / Renderer / GPU / Extension Host），安装 300 MB+。不是你们该付的税。
- **Grok**：已经选对壳（Tauri）。错在把半个操作系统塞进同一个 Host，又在 WebView 里再养 IDE + 浏览器 + 桌宠。

文件打开应对齐 Codex：探测 Cursor / VS Code 然后 `-g path:line`（`editors.rs` 已有），CodeMirror 降为只读预览。

---

## 2. Apple Design 怎么用在这个项目上

这份 skill **不是皮肤**。主线一句话：

> 界面像手：从屏幕上的**当前值**起步，继承手指速度，投影落点，随时能抓住反转。弹簧是工具。材质是一层。八原则决定**不做什么**。

### 2.1 平台表（Flexibility）

| 面 | macOS | Windows / Linux | 手机镜像 |
|----|-------|-----------------|----------|
| 材质 | 系统 Sidebar vibrancy **或** CSS blur，二选一 | 实心 `--bg-sidebar-solid`，禁止 64px blur | toolbar 一层 frost，内容滚到下面 |
| 运动 | 分栏弹簧，**临界阻尼 1.0，禁止 bounce** | 同一套物理；GPU 差则只动 `transform` / `opacity` | sheet + 甩动投影，允许 damping ~0.8 |
| 字体 | `-apple-system`，不嵌 SF Pro 文件 | Segoe UI，不硬嵌 SF | 系统 UI，间距走 `rem` |
| 手势 | 指针为主，分栏 1:1 | 同左 | Pointer Events + 橡胶边 + 速度交接 |
| 模态 | 继续 `GlassModal`，锚在触发源 | 同左 | 才允许 Vaul 类 sheet |

### 2.2 Skill 条款 × 仓库动作

| 条款 | 要求 | 现在 | 动作 |
|------|------|------|------|
| Response | pointer-down 立刻反馈 | `:active` 仅 8 条 | 共享 `.press`：`scale(0.97)` / 100ms |
| Direct 1:1 | 拖哪跟哪，认 grab offset | 分栏/终端/会话拖已有 capture | 补速度历史；松开走弹簧 |
| Interrupt | 动画中可反转，从现值起步 | CSS `width` transition 会跳 | **只**给 4 个手势面加 spring |
| Spring | 默认 damping 1.0；甩动才 0.8 | pane 320ms 三次贝塞尔、无过冲 | 桌面保持无过冲；bounce 仅手机 sheet |
| Spatial | 进从哪来回哪去 | 设置页与工作台原子切换 | 设置从左栏长出；菜单 `transform-origin: trigger` |
| Rubber-band | 越界阻尼 | 聊天在**防** WKWebView 橡胶边 | 分栏 min/max 用 `rubberband()`；聊天不跟系统抢 |
| Material | 一层半透明；禁止玻璃叠玻璃 | 壁纸 + vibrancy + 64px + 48px | 每窗一层。Win 实心。壁纸降 Companion |
| A11y | motion / transparency / contrast 三路 | 仅 15 处 reduced-motion | transparency → solid；contrast → 实边 |
| Type | tracking 随字号；系统字体 | 字体栈已对；spacing 散落 | `--track-display: -0.02em` · `--track-ui: 0.01em` |
| Purpose | 决定不做什么 | 默认面过宽 | Core / Companion 开关 |

### 2.3 材质层（现在在打架）

```
[壁纸/视频]     Companion，默认关（全屏运动违反 reduced-motion）
[原生 vibrancy] mac 保留 —— 这是系统材质
[侧栏 CSS 64px] 与 vibrancy 二选一，禁止双开
[模态 glass 48px] 留一层；无 backdrop 回落 solid（已有 token）
[聊天实心柱]    必须实心。字不写在玻璃上（vibrancy 规则）
```

WKWebView 约束：skill 写「进场同时动画 blur 和 scale」在这里是 GPU 炸弹。进场只动 **opacity + scale**；`--glass-blur` **固定**，禁止每帧改。

### 2.4 运动库（和轻量化不打架）

| 选项 | 决定 |
|------|------|
| `motion`（motion.dev） | **B2 开工才加**。只服务 4 个手势面。tree-shake 后数 KB |
| `framer-motion` | **禁止** |
| Vaul | **仅手机镜像**。桌面模态不改抽屉 |
| 纯 CSS | 颜色 / hover / 透明度 **继续 CSS**。Skill 只禁「手势驱动」用 transition |

四条手势面（仅此四条）：

1. 左栏宽度  
2. 右栏宽度  
3. 底栏终端高度  
4. 手机镜像 sheet  

公式（skill 原文，落地 `src/lib/motionSpring.ts`）：

```js
// 落点投影（不是 v²/2a）
project(v, d = 0.998) => (v / 1000) * d / (1 - d)

// 越界
rubberband(overshoot, dimension, c = 0.55) =>
  (overshoot * dimension * c) / (dimension + c * Math.abs(overshoot))
```

桌面弹簧参数：damping **1.0**，response **0.35–0.40**。手机 sheet 甩动：damping **0.8**，response **0.30**。

---

## 3. 产品裁剪（Purpose = 轻量化）

### Core（默认开）

项目 · 会话 · 流式对话 · 权限条 · 文件预览 · Diff · 模型 / 推理档 · 官方登录 / 中转

### Companion（默认关，设置里开）

桌宠 · 壁纸视频 · 皮肤市场 · Office 预览 · xterm 底栏 · 内嵌浏览器 · Remote IM · 语音

 Companion 未开启时：**Host 不初始化对应模块**（不只是 UI 隐藏）。

---

## 4. 双轨计划

两轨共享同一裁剪。任何让 Host 更重、让 WebView 更糊的「苹果味」直接否决。

### 轨 A · 架构（Codex 进程模型）

```
目标形态

  Tauri UI ──IPC── Session Host ──ACP stdio── grok CLI
       │
       └──按需── IM sidecar / 镜像 / 独立浏览器窗
```

| 刀 | 做什么 | 验收 |
|----|--------|------|
| A0 | IM / 镜像 / 桌宠 / 语音：未启用则不 `init` | 关 IM 后 Host 无对应任务/端口 |
| A1 | `App.js` 切设置 / 皮肤 / Office / xterm | 主包 &lt; 800 KB |
| A2 | minidump + WebView 崩溃回调 | 崩溃目录有 dump + 会话 id + CLI stderr ring |
| A3 | 镜像 HTTP 按需 spawn | 设置关则无监听 |
| A4 | 侧栏浏览器 → 独立 `WebviewWindow`，关掉主窗 `unstable` 子 webview | 浏览器崩、主窗还在；Win 焦点不再转 HWND |
| A5 | `remote_im` → Rust sidecar（**禁止**搬回 Node `remote-bridge`） | sidecar 挂了，聊天还在 |

保持 Tauri。不迁 Electron。不内嵌 grok CLI（产品 D4）。

### 轨 B · 界面（Apple Design 合同）

| 刀 | 做什么 | 不做什么 | 验收 |
|----|--------|----------|------|
| B0 | 本合同合入 `docs/llm-wiki/apple-motion.md` | 不改像素 | 新 PR 能对照否决「再加一层 glass」 |
| B1 | `.press` + `prefers-reduced-transparency` | 不加库 | 主路径按下即瘪；transparency 时 blur=0、opacity≥0.97 |
| B2 | 四条 spring + 速度交接 + rubberband | 不为菜单/设置/思考块加弹簧 | 拖到一半反向无跳变；reduced-motion → cross-fade |
| B3 | 每窗一层 blur；Win 实心；tracking token；设置从左栏出现 | 不动画 blur 半径；不嵌 SF Pro | `backdrop-filter` 声明降到 ≤12；玻璃上字加字重 |

---

## 5. 八周排期（一人全职量级）

顺序锁死：**先减层再加弹簧，先拆主包再加 motion。**

| 周 | 轨 A | 轨 B | 可演示 |
|----|------|------|--------|
| 1 | A0 未启用不 init | B0 合同合入 wiki | 关 IM 后 Host 变瘦；设计否决清单可引用 |
| 2 | A1 主包切片 | B1 `.press` + transparency | 冷启动主包下降；按钮按下即瘪 |
| 3 | A2 minidump | B3 材质：mac 二选一，Win 实心 | 侧栏不再双层霜；崩溃有 dump |
| 4 | A3 镜像按需 | B2 侧栏 / 右栏弹簧 | 拖分栏可中途反向，越界发黏 |
| 5 | A4 浏览器独立窗 | B2 底栏终端同一套 | 主窗与浏览器崩溃解耦 |
| 6 | A5 IM sidecar | 手机 sheet（可 Vaul） | IM 挂了聊天还在；手机可甩关 |
| 7 | Core / Companion 开关 | tracking + 设置空间路径 | 新装默认无桌宠无壁纸视频 |
| 8 | 指标复测 | 慢放回看运动；a11y 三路 | 对照 §6 打分，未达标不扩功能 |

可压缩到四周（1+2、3、4+5、6+7），**不可**先做 B2 再做 B3，也不可先加 `motion` 再切 `App.js`。

---

## 6. 验收数字

| 指标 | 现在（v0.2.30） | 目标 |
|------|-----------------|------|
| Windows NSIS | 15.4 MB | ≤ 18 MB（sidecar 可选下载） |
| `App.js` | 2.7 MB | &lt; 800 KB |
| `backdrop-filter` 声明 | 85 | ≤ 12，运行时每窗 ≤ 1 层 |
| `:active` / press | 8 条 | 按钮、列表行、chip、权限条全覆盖 |
| `prefers-reduced-transparency` | 无 | glass → solid token 回落 |
| 手势弹簧 | 0（CSS 320ms） | 4 条，可打断 |
| 默认驻留进程 | 1 × 全功能 Host | 1 UI + 1 Session；IM 零 |
| 崩溃现场 | code+address 一行 | minidump + session id + CLI stderr |
| IM 崩溃 | Host 一起没 | 仅 sidecar 重启 |

---

## 7. 明确禁止

| 动作 | 看起来像 | 实际 |
|------|----------|------|
| 迁 Electron 学 Cursor | 更稳 | 体积 ×20，养一套 Chromium |
| GPUI / Iced 重写 | 更原生 | 半年无产品 |
| 全站 Framer Motion | 处处弹簧 | 主包再肥；设置页弹违反 damping 1.0 |
| 桌面也用 Vaul sheet | 很 iOS | 破坏桌面熟悉度 |
| 再加一层毛玻璃 | 更高级 | 技能禁止叠层；WKWebView 已为此出过 bug |
| 嵌 SF Pro | 更像 Mac | 系统字体已有光学尺寸；涨安装包 |
| 把 grok CLI 打进安装包 | 开箱即用 | 违反 D4，Agent 与 UI 升级绑死 |
| 继续往 Host 塞通道/皮肤/浏览器 | 功能对标 | 崩溃半径继续涨 |

---

## 8. 代码落点（点头后才写）

| 路径 | 职责 |
|------|------|
| `docs/llm-wiki/apple-motion.md` | B0 合同：阻尼、材质层、平台表、禁止项（从本文 §2 抽出） |
| `src/styles/tokens.css` | `--press-scale` · `--track-display` · `--track-ui` · transparency 回落 |
| `src/lib/motionSpring.ts` | `project` / `rubberband` / 四条手势面共用 spring |
| `package.json` | **仅 B2 开工**加 `motion`；禁止 `framer-motion` |
| `src/styles/*.css` | 删叠层 blur；**不新增** `*.partN.css` |
| `src-tauri/src/remote_im/` | A5 迁 sidecar，Host 只留 IPC |
| `src-tauri/src/win_crash.rs` | A2 从 last-words 扩到 minidump |

切片 PR 命名：`feat/lite-a<n>-…` / `feat/feel-b<n>-…`。一 PR 一刀。`pnpm typecheck && pnpm test` + 相关 `cargo test`。用户可见行为同步 wiki。CHANGELOG 写 `[Unreleased]`。

i18n：en + zh（+ zh-TW 同 key）。无 `window.confirm`。浮层材质走 `docs/llm-wiki/dialogs.md`。

---

## 9. 拍板项（看完计划书只回这三句就行）

1. **平台策略**：桌面无 bounce / 手机才 sheet —— 接受还是要改？  
2. **Companion 默认关**：桌宠、壁纸视频、IM 不进新装默认面 —— 接受还是白名单要改？  
3. **开工顺序**：按 §5 从 A0+B0 起，还是指定先做某一刀？

未拍板前 **不改产品代码**。本文即计划书正文。
