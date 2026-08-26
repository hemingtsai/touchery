# Touchery

一个用 [GPUI](https://gpui.rs) + [gpui-component](https://github.com/longbridge/gpui-component) 构建的 macOS Spotlight 风格应用启动器。

常驻后台、菜单栏闪电图标、全局热键唤起、模糊/拼音搜索本地化应用名，并内置 **LuaJIT 插件系统**（支持二级输入）。

```
┌───────────────────────────────────────┐
│  🔍  搜索应用，或输入 > 调用插件…        │
├───────────────────────────────────────┤
│  计算器                                │
│  微信                                  │
│  Safari浏览器                          │
│  hello:一级执行: test                  │
└───────────────────────────────────────┘
```

## 功能特性

| 功能 | 说明 |
|---|---|
| 全局热键 | 默认 `⌘⇧Space`，可在控制面板自定义；按下唤起/再按关闭 |
| 常驻后台 | Accessory 模式，无 Dock 图标；菜单栏闪电图标提供「打开控制面板 / 退出」 |
| 应用搜索 | 模糊匹配 + 拼音全拼 + 拼音首字母，按系统 locale 显示本地化名称（中文系统显示「微信」「计算器」） |
| 插件系统 | LuaJIT 脚本，`>` 前缀路由，支持二级输入 |
| 控制面板 | 快捷键录制（即时生效）、插件列表与开关管理 |
| 性能 | 应用索引启动时后台加载一次；查询为纯内存过滤 + LuaJIT 执行；窗口按需创建，唤起无冷启动 |

## 系统要求

- macOS 12+
- Rust stable（edition 2024）
- **完整版 Xcode**（含 Metal 编译器，Command Line Tools 不够）

> 本仓库 `.cargo/config.toml` 已配置 `DEVELOPER_DIR=/Applications/Xcode-beta.app/Contents/Developer`。
> 如你的 Xcode 路径不同（如 `/Applications/Xcode.app`），请修改该文件，或全局执行：
>
> ```bash
> sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
> ```

## 构建

```bash
# 开发调试
cargo build

# 发布构建（lto=fat, codegen-units=1）
cargo build --release

# 运行
./target/release/touchery

# 打包 DMG 安装镜像（app bundle + icns 图标 + ad-hoc 签名）
./scripts/build_dmg.sh
# 产物: target/touchery-<version>.dmg
```

安装：打开 DMG → 将 Touchery.app 拖入 Applications → 启动后常驻菜单栏。

## 使用指南

### 启动器

| 操作 | 按键 |
|---|---|
| 唤起 / 关闭 | `⌘⇧Space`（可自定义） |
| 选择结果 | `↑` / `↓` |
| 执行选中项 | `Enter` |
| 关闭窗口 | `Esc` |

- 直接输入 → 模糊搜索应用。支持英文（`saf`）、拼音全拼（`jisuanqi`）、拼音首字母（`jsq`），且同时匹配本地化名与原始 bundle 名
- 输入以 **`>` 开头** → 路由到所有已启用插件（剥离 `>` 后作为 query 传入），带 80ms 防抖
- 选中标记为「需二级输入」的插件条目回车 → 进入二级输入模式（卡片收缩为单个输入框，`Esc` 返回），再次 `Enter` 将二级内容传给插件

### 控制面板

从菜单栏闪电图标 → 「打开控制面板」打开：

- **修改快捷键**：点击按钮进入录制态 → 按下任意组合键（至少一个修饰键 ⌘/⌥/⌃）→ 即时注册生效并保存；`Esc` 或再点按钮取消录制
- **插件管理**：列出已加载插件及状态（运行中 / 已停用 / 出错原因），Switch 开关即时启用/停用并持久化
- **关闭**：右上角按钮或 `Esc`

### 菜单栏图标

闪电模板图，自动适配深浅色菜单栏。左键点击弹出菜单：

- 打开控制面板
- 退出

## 配置文件

路径：`~/Library/Application Support/touchery/config.json`

```json
{
  "hotkey": {
    "mods": ["super", "shift"],
    "key": "space"
  },
  "plugins": {
    "hello.lua": true
  }
}
```

- `hotkey.mods`：`super` / `shift` / `ctrl` / `alt` 的组合
- `hotkey.key`：`space`、`a`–`z`、`0`–`9`、`f1`–`f12`、`enter`、`tab`、方向键等
- `plugins`：文件名 → 是否启用；缺失的条目默认启用

## 插件系统

### 安装位置

```
~/Library/Application Support/touchery/plugins/
```

将 `.lua` 文件放入该目录，重启应用即自动加载。每个插件拥有独立的 Lua runtime，互相隔离。

### API 契约

每个插件必须实现以下全局函数（缺任一必需函数则加载失败并在控制面板显示原因）：

```lua
-- 可选：声明式注释不影响功能

-- 函数一【必须】：查询时返回要显示的条目
-- query: 用户输入（">" 前缀已剥离、已 trim）
function get_items(query)
    return {
        -- title: 显示标题（UI 中渲染为 "<插件名>:<title>"）
        -- value: 回传给 run/run_sub 的数据（缺省等于 title）
        -- sub:   true 表示该条目需要二级输入（默认 false）
        { title = "问候",   value = "hello", sub = false },
        { title = "回声",   value = "echo",  sub = true  },
    }
end

-- 函数二【必须】：一级回车（sub=false 的条目被确认时调用）
function run(value, query)
end

-- 函数三【必须】：二级输入回车后调用
-- value: 原 item 的 value 字段; sub_query: 用户二级输入的内容
function run_sub(value, sub_query)
end
```

### 行为细节

- **触发**：仅当用户输入以 `>` 开头时调用插件的 `get_items`；否则只搜本地应用
- **防抖**：连续输入 80ms 后才发起插件查询；过期结果自动丢弃（generation 计数）
- **执行预算**：每次 Lua 调用限制 2000 万条指令（约几十毫秒级），超限中断并记录错误——失控脚本不会冻结 UI
- **隔离**：单插件出错只影响自身（状态显示在控制面板），不影响主程序和其他插件
- **执行环境**：`run` / `run_sub` 在后台线程运行，完成后窗口自动关闭
- 标准库可用（`os.execute`、`io` 等），可发起子进程/网络请求（自行封装 curl 等）

### 示例

参见 [`examples/hello.lua`](examples/hello.lua)：

```bash
cp examples/hello.lua ~/Library/Application\ Support/touchery/plugins/
# 重启 touchery 后，唤起启动器输入 "> " 即可看到插件条目
```

验证日志写入 `~/Library/Application Support/touchery/plugin.log`。

## 数据目录总览

| 路径 | 内容 |
|---|---|
| `~/Library/Application Support/touchery/plugins/*.lua` | 插件 |
| `~/Library/Application Support/touchery/config.json` | 快捷键 + 插件开关状态 |
| `~/Library/Application Support/touchery/plugin.log` | 示例插件输出（仅示例写入） |

## 架构

```
src/
├── main.rs       # 入口：全局状态(GPUI Global)、热键轮询循环、托盘、窗口生命周期、
│                 # accessory 策略、原生阴影禁用、活动屏幕定位
├── apps.rs       # mdfind -attr kMDItemDisplayName 枚举应用（本地化名）+ 拼音索引
├── search.rs     # 模糊子序列评分器（连续/词首/大小写驼峰加分）
├── hotkey.rs     # global-hotkey 封装、按键字符串 ↔ Carbon Code 映射、显示格式化
├── config.rs     # serde_json 配置持久化
├── tray.rs       # tray-icon 菜单栏图标（代码光栅化闪电模板图）
├── launcher.rs   # 启动器视图：自建搜索栏(放大镜图标)、ListState 键盘导航接管、
│                 # 二级输入模式状态机、插件后台查询合并
├── ui_settings.rs# 控制面板：observe_keystrokes 快捷键录制、插件开关
└── plugins.rs    # PluginManager：mlua(luajit+vendored) 加载/卸载、指令预算 hook
examples/hello.lua  # 示例插件
examples/gen_icon.rs# 应用图标光栅化工具（打包用）
scripts/build_dmg.sh# DMG 打包脚本
.cargo/config.toml  # DEVELOPER_DIR 指向 Xcode-beta（Metal shader 编译需要）
```

### 关键实现决策

- **热键 manager 保活**：`GlobalHotKeyManager` 存入 GPUI Global；换键复用同一 manager（unregister→register），避免重复创建失败
- **窗口生命周期（方案 A）**：热键触发时创建窗口、关闭时销毁；应用索引常驻内存，唤起开销 ~10ms 无感知
- **窗口内自毁**：事件回调中直接 `window.remove_window()` 并清除句柄，避免嵌套 `handle.update` 静默失败
- **透明窗口**：Root 包装保留（gpui-component 内部强依赖），但把全局主题背景设为 alpha=0；并用 FFI `[NSWindow setHasShadow:NO]` 关闭原生阴影，防止透明区域显形
- **键盘接管**：↑/↓ 利用单行 Input 未处理的 MoveUp/MoveDown 绑定落到 raw key listener 的特性接管方向键；快捷键录制使用 `observe_keystrokes` 全局观察者，无视焦点与组件绑定
- **二级输入不缩放窗口**：macOS 无法运行时移动窗口原点，缩高会导致顶边跳位；改为卡片自适应 + 关闭原生阴影

## 已知限制

- ad-hoc 签名的 DMG 首次打开需右键 →「打开」绕过 Gatekeeper；正式分发需 Developer ID 签名 + 公证
- `Cmd+Space` 被 Spotlight 占用，默认热键为 `Cmd+Shift+Space`；若与其他软件冲突，注册会失败并在控制面板提示
- 插件路由为全局统一前缀 `>`，暂不支持 per-plugin 前缀
- 插件的 `get_items` 结果不支持异步流式更新（一次性返回）

## License

Apache-2.0
