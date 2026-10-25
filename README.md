# RustTerm Egui

**纯 Rust 终端客户端**（egui 原型，目标：替代 Tauri + xterm.js 前端的主工程）。

技术栈：**egui 0.36（eframe/glow 渲染）+ alacritty_terminal 0.26（VT 状态机）+ vendor russh（SSH）+ portable-pty（本地 PTY）+ tokio + wry 0.47（WebView2 内置浏览器）+ ironrdp / vnc-rs / capsaicin（远程桌面）**。

代码位于主仓库之外（`rustterm-egui`），不改动主工程任何文件；
仅共享 `vendor/russh`，SSH 后端与主工程 `src-tauri/src/ssh.rs` 同一套 API 用法
（连接 / 认证 / channel select 事件循环照搬），旧算法（SHA-1/CBC/des）兼容性一致，可连 Jumpserver / 老设备。

> English version: [README_EN.md](README_EN.md)

---

## 功能特性

### 终端与会话
- **本地终端**（portable-pty + ConPTY）、**SSH 终端**（密码 / 密钥认证，含私钥选择器与 passphrase）
- **ProxyJump 跳板机**：真实 `direct-tcpip` 通道转发，跳板机 Handle 保活，凭据沿用目标机认证方式
- **Telnet**（原始 TCP + 最小 IAC 协商）、**Ping**（PTY 跑 `ping.exe -t`）
- **会话管理**：`sessions.json` 持久化、新建/编辑/删除、最近会话、快速连接栏（地址|跳板机|认证方式|凭据|连接）、新建会话「🔗 连接」一键保存并连接
- **批量执行**（命令发到所有终端）、**同步输入**（活动终端输入镜像到其它终端）、**隧道**（本地 TCP 监听 → 目标中转，可启停/看连接数）、**查找**（Ctrl+F 匹配高亮 + 上下跳转）、**键宏**（编码字节流写入活动终端）
- 双击/右键连接侧栏会话；统一标签条（图标 + 标题 + 关闭 + 拖拽排序 + 双击重命名）

### SFTP
- 面板绑定**当前活动 SSH 会话**，复用已认证连接（不重新建链、不重复认证，跳板机场景成立）
- 远端目录浏览、上传/下载（64KB 分块 + 进度 + 可取消）、重命名/新建目录/删除、隐藏文件过滤
- **拖拽上传**：文件拖到面板上释放即上传，拖入过程整块高亮
- 操作完成（上传/删除/新建目录/重命名）后**自动刷新**列表；显示隐藏文件开关不重复列目录

### 远程桌面（VNC / RDP / SPICE）
- 三协议真实实现，统一接口（`RdCommand` 输入 / `RdEvent` 帧回调），RGBA8 脏矩形增量渲染
- VNC 安全协商两轮握手（带凭据失败退回无凭据）；RDP 经 CredSSP；错误信息将被 `friendly_error()` 翻译成可操作提示
- 键映射：扫描码 + X11 keysym，`printable_keysym` 支持 Shift 大写/符号；修饰键差分合成；无映射键走 Unicode 通道
- `fit`/`zoom` 缩放、捕获键盘、指针/滚轮转发；从已保存会话打开自动连接

### 内置浏览器
- wry / WebView2 原生子窗口，每标签独占一个 WebView；导航/标题经回调同步地址栏
- 书签 / 历史（共享 BrowserService）、后退/前进/刷新
- 布局与 AI 面板并排兼容（WebView 区域右缘钳制到 AI 面板左缘）

### AI 助手
- 多提供方：OpenAI 兼容（覆盖中转网关/GLM/Kilo）/ Ollama 本地 / Anthropic，配置持久化
- **自动附带终端上下文**：发送时拼接【终端信息】头（SSH user@host / Telnet / 本地）+ 最近 N 行终端输出，用户气泡直接可见完整发送内容
- **附件**：📷 截图（GDI 抓客户区）、📎 文件（文本内联 64KB 截断 / PNG-JPEG 图片走视觉通路）、支持拖文件进窗口
- Agent 开关、只读模式拦截、回复命令提取「发送到终端」、对话历史限量

### 通用
- **中英文切换**（~90 词条静态表，未知 key 原样返回）、**深色/浅色主题**实时生效
- 所有密码框带 👁 眼睛按钮（明文切换）、逐位状态存 egui Memory
- 弹窗全部居中；窗口尺寸按工作区自适应（`SPI_GETWORKAREA` + DPI）
- 持久化主路径失败自动回退 exe 旁 `data\`，状态栏 toast 提示

---

## 构建 / 运行

```powershell
cd rustterm-egui
cargo run --release   # 推荐；debug 可直接跑（依赖按 opt-level=2 编译）
cargo test            # sftp_client 工具函数 + telnet IAC 单测
```

- 开发机的编译检查用 `check-msvc.bat`（封装 vcvars64 + INCLUDE）
- `russh` 关闭 default-features（避免 aws-lc-rs），`debug=0` 保持体积

## 架构

```
src/
  main.rs        eframe 入口（glow 后端）+ 窗口自适应
  app.rs         外壳：菜单栏 + 工具栏 + 快速连接 + 标签条 + 中央面板 + 状态栏
  term.rs        核心：Term<TermPane::PtyProxy> 状态机、逐格绘制（Painter）、键盘/鼠标→VT 编码、
                 选区复制、滚动回看、查找高亮、IME 预编辑
  tabs.rs        统一标签模型：Connecting / Terminal / Browser / RemoteDesktop / LocalFiles / Failed
  sessions.rs    会话模型 + sessions.json 持久化 + fallback 落盘
  config.rs      AppConfig（含 AiSettings，serde default 迁移旧配置）
  fonts.rs       系统字体注入（Consolas 主体 + 微软雅黑 CJK 兜底）
  backends/
    ssh.rs       russh：密码/密钥认证、ProxyJump（direct-tcpip）、SshHandle.open_sftp()
    local.rs     portable-pty 本地 shell
    tcp.rs       原始 TCP（Telnet，最小 IAC 协商）
  sftp.rs        SFTP 右侧停靠面板（绑定活动 SSH 会话 + 拖拽上传）
  sftp_client.rs SftpSession 封装：列表/传输/取消/进度 + 路径工具
  remote/        VNC（vnc-rs vendor）/ RDP（ironrdp）/ SPICE（capsaicin）统一接口
  rdp.rs         远程桌面标签页渲染（ColorImage + TextureHandle 增量更新）
  browser.rs     wry WebView 子窗口（惰性创建、可见性同步、边界钳制）
  ai.rs          多提供方请求构造/响应解析 + 命令提取/危险判定
  ai_assistant.rs 对话面板（气泡 / 附件 / 终端上下文）
  ai_config_panel.rs AI 配置面板（保存 → config.save()）
  screenshot.rs  GDI 截屏（BitBlt → PNG → base64）
  tunnel.rs      本地 TCP 隧道中转
  filebrowser.rs 单栏本地文件浏览器（主标签页）
  macros.rs      键宏（组合键→控制码字节流）
  i18n.rs        中英文静态词典
  widgets.rs     通用组件（password_field 眼睛按钮等）
```

**数据流**：`pty/ssh → mpsc → vte Processor.advance(Term) → renderable_content() → egui Painter`；
输入方向：`egui 事件 → VT 序列 → mpsc → pty/ssh`（终端设备查询响应经 `PtyProxy` 回写）。

---

## 近期变更（开发日志摘录）

| 时间 | 内容 |
|---|---|
| 10-24 | 弹窗/表单/浏览器/键宏/主题七轮修复；按 XShell 风格重构 UI（会话侧栏 + 欢迎页 + 状态栏）；内置浏览器真实化（wry 子窗口）；统一标签页 + AI 多提供方；快速连接密码/密钥 + 窗口尺寸修复；真实 ProxyJump + 远程桌面三协议 + SFTP 侧栏；SFTP 改真远程浏览；语言切换落地 + 菜单调整 + 键盘捕获修复 + SFTP 自动弹出 |
| 10-25（rc1） | 修复远程桌面功能键、会话保存/AI 保存、浏览器遮挡 AI 面板（WebView 右缘钳制）；AI 问答自动带终端信息头 + 最近输出 |
| 10-25（rc2） | AI 支持文件/图片附件与拖拽；SFTP 拖拽上传（系统光标坐标换算修复落点判定）；SFTP 上传/删除/新建/重命名完成后自动刷新 |

## 开发备忘（egui / wry 踩坑）

1. `egui::Panel` 按 `show()` 顺序占用父区域，`CentralPanel` 必须最后 show；
2. 自绘控件必须 `Sense::click()`（`Sense::hover` 的 `clicked()` 恒 false）；
3. `wry::Rect` 用 `LogicalPosition`；`SetWindowPos` 设**子窗口**用父客户区坐标，不要叠加屏幕偏移；
4. `load_html` 产生 `data:html` 导航，导航回调必须过滤；
5. WebView2 子窗口永远盖在 egui 之上：任何浮层打开时先整体隐藏 WebView（`overlay_open()`）；
6. WebView2 创建后抢系统焦点，需要 `SetFocus(主窗口)`；
7. 输入框不能绑定临时变量，必须落在结构体字段，且聚焦时不被真实值覆盖；
8. 同一 Ui 内多个 `ScrollArea` 要 `.id_salt()`；
9. OLE 拖放期间 egui 指针停在拖拽前旧值（winit 忽略 DragOver 位置），落点判定要用 `GetCursorPos` + `ScreenToClient` 换算；
10. egui 0.36 的 `CentralPanel` 配可拖宽右侧面板时可能未完全让位：对原生子窗口区域按同帧面板矩形钳制。

## 已知缺口

- SSH 主机密钥校验：`check_server_key` 无条件接受（正式版需移植主工程 `hostkey.rs` 策略）
- VNC 暂不支持 CapsLock 状态同步；拖拽上传仅支持文件（目录需给 `sftp_client` 加递归）
- AI 附件仅文本 / PNG-JPEG；其它二进制不含
- 性能：逐格 `painter.text` 每帧重排 galley，可整行合并或 wgpu 顶点缓存
- 动态调色板：256 色用静态 xterm 色表，未接 `term.colors()` / 主工程换肤

## 与主工程关系

- 不共享代码，仅共享 `vendor/russh` 并参考 `src-tauri/src/ssh.rs` 的 API 用法
- 若本原型各项验证（渲染性能 / CJK / 键鼠输入 / SFTP / 远程桌面 / 浏览器 / AI）达标，
  可将 `term.rs` 绘制与输入编码层迁入主工程替换 xterm.js 前端
