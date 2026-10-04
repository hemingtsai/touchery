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
| 开机启动 | 控制面板开关，基于用户 LaunchAgent（`~/Library/LaunchAgents/com.touchery.app.plist`） |
| 应用搜索 | 单一相似度打分：前缀/缩写优先，容忍少字、多字、错字与相邻换位；**汉字可直接输入**，也可打拼音全拼或首字母；按系统 locale 显示本地化名称（中文系统显示「微信」「计算器」） |
| 习惯排序 | 记录每个应用的启动次数与上次使用时间，在相似度之上加成（默认最多 5%）；空查询直接按「最近 / 最常用」排列 |
| 插件系统 | LuaJIT 脚本，`>` 前缀路由，支持二级输入；控制面板「安装插件…」可直接从文件选择框安装 |
| Lua 主题 | 亮暗色定义在同一个 `.lua` 文件中，控制面板选择，未定义颜色回退内置主题，自动跟随系统深浅色 |
| 控制面板 | 快捷键录制（即时生效）、搜索调参滑块、使用记录与清除、插件列表与开关管理、主题选择 |
| 性能 | 应用索引启动时后台加载一次；查询为纯内存过滤 + LuaJIT 执行；窗口按需创建，唤起无冷启动 |

## 系统要求

- macOS 12+
- Rust stable（edition 2024）
- **完整版 Xcode**（含 Metal 编译器，Command Line Tools 不够）

> 构建使用系统当前选择的 developer 目录。若 `xcode-select -p` 指向的不是完整版 Xcode，请切换：
>
> ```bash
> sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
> ```
>
> 或仅为本次构建指定：`DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer cargo build`。

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

窗口在**失去焦点时自动关闭**（点击其他应用、`Cmd`+`Tab` 切换、唤起控制面板等），无需手动按 `Esc`。

- 直接输入 → 按相似度排序搜索应用。支持英文（`saf`）、拼音全拼（`jisuanqi`）、拼音首字母（`jsq`）以及**直接输入汉字**（`计算器`、`微信`），同时匹配本地化名与原始 bundle 名（bundle 名排在显示名之后）
- **容错**：少打字母（`wixin` → 微信）、多打字母（`safarii` → Safari）、打错字母、相邻两字母打反（`gmial` → Gmail，拼音如 `jiusanqi` → 计算器）都能命中，错误越多分数越低，每多一对换位再降一档
- **阈值**：1 个字符只匹配词首（`s` 命中 Safari，`a` 不命中；`微` 命中微信，`信` 不命中）；2 个字符允许缩写；3 个及以上允许一处错拼。低于阈值的条目不显示
- **中文**：汉字名同时索引「拼音」和「原字」，所以 `jisuanqi`、`jsq`、`计算器` 都能命中计算器；中文查询同样支持前缀与子串（`计算`、`算器`、`计器` 都命中），中英混排（如 `微n`）也能匹配
- 输入包含 **`/`** → 层级路径搜索：按 `/` 分段依次匹配「本地化文件夹 → 应用」，段顺序不可颠倒，各段支持同样的容错。例如 `shiyong/cipan` 命中「实用工具/磁盘工具」。**只有带 `/` 的查询会匹配文件夹**：单独输入 `shiyong` 不会列出实用工具内的应用
- **习惯**：每个应用记录启动次数与上次使用时间（`usage.json`），在相似度上最多加成「习惯加成上限」（默认 50/1000）。因此常用应用的 bundle 名匹配可以超过一个没怎么用过的精确匹配，但不会越过相差一整档的相关性，也不会救回低于阈值的条目
- **空查询**：刚唤起时没有文本可匹配，列表按习惯排序——最近用过的最靠前，没用过的保持字母序
- **输入法**：默认打开「输入法支持」时，唤起会临时取得前台（非激活面板里系统输入法无法组字），关闭启动器时把键盘还给原来那个应用，所以中文/日文等输入法的候选词能直接在输入框里组字；关掉该开关则不占用前台——启动器本来就在前台时（例如刚用过控制面板）会先把键盘交还给上一个应用，于是中文输入法不再作用于启动器，输入框回到只接受英文与拼音的状态
- 输入以 **`>` 开头** → 路由到所有已启用插件（剥离 `>` 后作为 query 传入），带 80ms 防抖
- 选中标记为「需二级输入」的插件条目回车 → 进入二级输入模式（卡片收缩为单个输入框，`Esc` 返回），再次 `Enter` 将二级内容传给插件

### 控制面板

从菜单栏闪电图标 → 「打开控制面板」打开（标准窗口，**左侧导航 + 右侧内容**，780×580 起始、可调整大小，最小 620×420，`Esc` 或红点关闭）。窗口在**鼠标所在的那块屏幕**上居中打开（光标不在任何屏幕上时回落到主屏），而不是固定落在主屏的某个偏移处：

页面从左侧导航切换，右侧显示当前页面，右上角显示当前唤起快捷键：

- **通用**：「登录时自动启动」开关（写入/移除用户 LaunchAgent；关闭时只删除 plist，下次登录不再启动，当前进程继续运行；移动应用位置后需重新开启）、「仅搜索应用程序」开关（只索引 Application 文件夹内的应用，排除系统深处的 helper，下次唤起生效）；「输入法支持」开关（是否为了系统输入法临时取得前台，详见「启动器」一节的「输入法」）
- **快捷键**：点击按钮进入录制态 → 按下任意组合键（至少一个修饰键 ⌘/⌥/⌃）→ 即时注册生效并保存；若被其他应用占用则显示错误并恢复原快捷键；`Esc` 或再点按钮取消录制
- **主题**：列出内置主题与 `themes` 目录下所有 `.lua` 主题，点击即时切换并持久化
- **仅搜索应用程序**：开启后索引范围限定在 Application 文件夹内的用户级应用（排除系统深处的 helper），并支持 `/` 分段的路径层级搜索；不影响插件路由（下次唤起启动器生效）
- **搜索**：七个滑块（单/双/三字符及以上阈值、中段命中权重、换位罚分、bundle 名称权重、习惯加成上限），拖动即时生效并写入配置，另有「恢复默认」
- **使用记录**：列出习惯最强的 10 个应用（启动次数 · 上次使用），「清除使用记录」会清空内存与 `usage.json`；导航项右侧显示已记录的条数
- **插件**：列出已加载插件及状态（运行中 / 已停用 / 出错原因），Switch 开关即时启用/停用并持久化；「安装插件…」按钮弹出系统文件选择框，选中 `.lua` 后立即安装并加载（同名不覆盖，顺延 `name-2.lua`）；导航项右侧显示插件数
- **关闭**：红点或 `Esc`

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
- `ime`：是否支持系统输入法（默认 `true`）。打开时启动器唤起会临时取得前台并在关闭时归还，中文/日文等输入法才能在输入框组字；关闭后输入框只接受英文与拼音
- `search`：控制面板「搜索调参」的七个值，单位与打分一致（1000 = 满分）。缺省或越界时按默认值/上限处理，手改不会让搜索失效：
  `threshold_1` 950、`threshold_2` 750、`threshold_3` 600、`match_mid` 800、`pen_transpose` 500、`bundle_weight` 980、`usage_boost_max` 50

## 使用记录

`usage.json` 只记录本机行为，不联网：每个条目是「应用路径 → 启动次数 + 上次使用时间 + bundle 名」。写入是原子的，保存时只保留使用最多的 500 条，并且丢弃 90 天前只启动过一次的记录；文件缺失或损坏都按「没有记录」处理，不会影响设置。控制面板「使用记录」里可以查看与清除，也可以直接删除该文件。

按路径记录意味着：应用改名或搬到别处后，原来的历史不会自动跟随（会重新从零开始累积）。

## 插件系统

### 安装位置

```
~/Library/Application Support/touchery/plugins/
```

（数据目录由 `dirs::data_dir()` 决定，不可用时回退到 `$HOME/Library/Application Support`；两者都不可用则禁用插件加载，绝不回退到当前工作目录。）

将 `.lua` 文件放入该目录，重启应用即自动加载。每个插件拥有独立的 Lua runtime，互相隔离。

### API 契约

每个插件必须实现以下全局函数（缺任一必需函数则加载失败并在控制面板显示原因）：

```lua
-- 可选：插件自述，控制面板会显示这些信息；字段都可省略，name 缺省用文件名。
-- repository 只在 http(s) 时保留，点击「仓库」会用系统默认浏览器打开。
PLUGIN = {
    name = "计算器",
    version = "1.0.0",
    author = "hemingtsai",
    license = "MIT",
    repository = "https://github.com/hemingtsai/touchery",
    description = "四则运算 + 次方",
}

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

-- 函数三【必须】：二级输入回车后调用（sub=true 的条目确认后进入二级输入）
-- value: 原 item 的 value 字段; sub_query: 用户二级输入的内容
function run_sub(value, sub_query)
end
```

> 三个函数均为必需：缺少 `get_items`、`run` 或 `run_sub` 中任何一个，插件都会加载失败，原因显示在控制面板的插件状态中。

### 行为细节

- **触发**：仅当用户输入以 `>` 开头时调用插件的 `get_items`；否则只搜本地应用
- **防抖**：连续输入 80ms 后才发起插件查询；被后续输入取代的查询直接取消，不会排队执行
- **执行预算**：每次 Lua 调用限制 2000 万条指令，另有 5 秒兜底上限（先到者为准），超限中断并记录错误。LuaJIT 编译被关闭，因此该预算对普通循环、协程以及插件顶层初始化代码一律生效；阻塞式 C 调用（如 `os.execute`）无法被中断，此时只有后台查询线程被占用，UI 不会卡死
- **停用**：停用的插件在启动时不会执行顶层代码，直到在控制面板重新启用
- **隔离**：单插件出错只影响自身（状态显示在控制面板），不影响主程序和其他插件
- **执行环境**：`run` / `run_sub` 在后台线程运行，完成后窗口自动关闭
- 标准库可用（`os.execute`、`io` 等），可发起子进程/网络请求（自行封装 curl 等）；`jit` 全局不可用

### 安装

控制面板 →「插件」→「**安装插件…**」：弹出系统文件选择框，选中一个 `.lua` 文件即完成安装，**无需重启**（立即加载，加载失败的原因显示在该插件的行上）。

- 同名文件不会被覆盖，会顺延为 `name-2.lua`、`name-3.lua`，避免覆盖你已经改过的脚本；状态行会告诉你实际装成的文件名
- 如果新插件的 `PLUGIN.name` 或 `PLUGIN.repository` 与已安装的某个插件相同，状态行会点名提示（两个都会保持启用，由你决定停用哪个）
- 插件列表每行显示：自述名称与版本、`文件名 · 作者 · 许可证`、说明文字，以及可点击的「仓库」

也可以手动安装：把 `.lua` 文件放进插件目录，然后重启应用。

### 示例

参见 [`examples/hello.lua`](examples/hello.lua)（插件 API 契约）与 [`examples/calc.lua`](examples/calc.lua)（计算器：输入 `> 1+2^3` 实时显示结果，回车把结果复制到剪贴板）：

```bash
mkdir -p ~/Library/Application\ Support/touchery/plugins
cp examples/hello.lua ~/Library/Application\ Support/touchery/plugins/
# 重启 touchery 后，唤起启动器输入 "> " 即可看到插件条目
```

> 插件目录不会自动创建；目录不存在时不会加载任何插件，插件列表为空。用控制面板的「安装插件…」按钮安装时会自动创建。

验证日志写入 `~/Library/Application Support/touchery/plugin.log`。

## 主题系统

### 安装位置

```
~/Library/Application Support/touchery/themes/*.lua
```

放入 `.lua` 文件后**重启应用**加载，然后在控制面板「主题」区域点击选择。

### 文件格式

亮暗色定义在**同一个文件**中；文件必须 `return` 一个 table：

```lua
return {
    name = "暖色纸感",   -- 可选，控制面板显示名（缺省用文件名）

    light = {            -- 系统浅色外观时生效
        card_bg        = "#faf6efF2",
        text_primary   = "#2b2620",
        accent_error   = "#c2452d",
        -- ...其余键见下表
    },

    dark = {             -- 系统深色外观时生效
        card_bg        = "#241f1aF0",
        -- 未定义的键回退到内置暗色主题
    },
}
```

### 规则

- **颜色格式**：`#rgb`、`#rrggbb`、`#rrggbbaa`（最后两位为透明度）
- **回退**：未定义的单个颜色 → 回退到内置主题对应模式的值；整个 `light`/`dark` 表缺失 → 该模式完全使用内置值；两个表都空的主题文件会被跳过
- **动态响应**：调色板在每帧渲染时按当前系统外观解析——macOS 切换深浅色后自动套用对应子表，无需重启
- **未知键 / 非法颜色**：忽略并记录日志，不影响其他键
- **文件名**：`builtin`（即 `builtin.lua`）保留给内置主题，该文件会被拒绝加载
- **执行预算**：主题文件与插件一样受执行预算约束（2000 万条指令，5 秒兜底），失控的主题文件只会被跳过，不会阻塞启动

### 可用颜色键

| 键 | 用途 |
|---|---|
| `card_bg` | 启动器卡片背景 |
| `card_border` | 启动器卡片边框 |
| `panel_bg` | 控制面板背景 |
| `row_bg` | 列表行 / 悬停高亮背景 |
| `input_bg` | 输入框背景 |
| `input_border` | 输入框边框 |
| `text_primary` | 主文字 |
| `text_secondary` | 次要文字 / 提示 |
| `accent_info` | 信息蓝（录制态边框、当前项标记） |
| `accent_ok` | 成功绿（插件运行状态） |
| `accent_error` | 错误红（失败提示） |

示例参见 [`examples/theme_example.lua`](examples/theme_example.lua)。

## 数据目录总览

| 路径 | 内容 |
|---|---|
| `~/Library/Application Support/touchery/plugins/*.lua` | 插件 |
| `~/Library/Application Support/touchery/themes/*.lua` | 主题 |
| `~/Library/Application Support/touchery/config.json` | 快捷键 + 插件开关 + 当前主题 + 搜索调参 |
| `~/Library/Application Support/touchery/usage.json` | 使用记录（启动次数 + 上次使用时间），控制面板可清除 |
| `~/Library/LaunchAgents/com.touchery.app.plist` | 开机启动 LaunchAgent（由控制面板管理） |
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
├── ui_settings.rs# 控制面板：observe_keystrokes 快捷键录制、插件开关、主题选择
├── plugins.rs    # PluginManager：mlua(luajit+vendored) 加载/卸载、指令预算 hook
├── themes.rs     # Lua 主题：加载/解析/回退解析，按系统外观每帧解析调色板
└── ui_theme.rs   # 窗口尺寸常量
examples/hello.lua  # 示例插件
examples/gen_icon.rs# 应用图标光栅化工具（打包用）
scripts/build_dmg.sh# DMG 打包脚本
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
- `Cmd+Space` 被 Spotlight 占用，默认热键为 `Cmd+Shift+Space`；若与其他软件冲突，注册会失败——应用仍会启动（托盘可用），错误显示在控制面板的快捷键区域
- 插件路由为全局统一前缀 `>`，暂不支持 per-plugin 前缀
- 插件的 `get_items` 结果不支持异步流式更新（一次性返回）
- 插件的执行预算只能中断 Lua 本身；阻塞在 C 调用（如 `os.execute` 等待外部进程）里的插件仍会一直占用插件执行线程，此时控制面板会继续显示上一次的插件状态而不是卡死
- 习惯排序是刻意的近似：常用应用的 bundle 名匹配可以超过一个完全没用的精确匹配（默认最多 5% 加成）；想完全按文本排序，把「习惯加成上限」调到 0
- 使用记录按应用路径保存，改名/搬移后会从零开始累积
- 目录变更监听只覆盖 `/Applications` 与 `~/Applications`（含启动时不存在的 `~/Applications`）；装在别处的应用会在下次启动，或上述目录发生变更触发全量重扫时进入索引
- 窗口初始位置会按目标屏幕尺寸收缩并钳位，但控制面板的最小尺寸（420×320）是固定值；若显示器本身比它还小，macOS 仍会按最小尺寸把窗口撑出边界
- 不监听显示器配置变化（拔插 / 改主屏 / 调整排列）。启动器每次唤起都会重算位置，不受影响；控制面板沿用已打开的窗口，macOS 会在显示器消失时把它挪到剩余屏幕上，但若仅是重新排列导致窗口落到屏幕外，需手动拖回。macOS 不支持运行时移动窗口原点，唯一「修好」的办法是销毁重建窗口，会丢失面板状态，故不做

## License

Apache-2.0，全文见 [`LICENSE`](LICENSE)。
