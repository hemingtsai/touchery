-- touchery 计算器插件（示例）
-- 安装：控制面板 →「插件」→「安装插件…」，或手动放进 plugins/<作者>/ 目录
-- 使用：唤起启动器后输入 "> 1+2^3*4"，结果实时显示，回车把结果复制到剪贴板
--
-- 支持：+ - * / % ^ ( ) 一元正负号、小数与科学计数、常量 pi / e、
--       一元函数 sqrt abs floor ceil round sin cos tan log log10 exp deg rad
--       优先级 ^（右结合）> 一元负号 > * / % > + -，例如 -2^2 = -4、2^3^2 = 512；
--       一元符号按数学惯例允许叠加，所以 1+-2 = -1、1--2 = 3
--
-- 契约（见 examples/hello.lua）：
--   get_items(query)          -> { { title, value, sub }, ... }
--   run(value, query)         -> 一级回车（sub=false 的条目）
--   run_sub(value, sub_query) -> 二级输入回车后调用（本插件不使用，但契约要求定义）
--
-- PLUGIN 必须是字面量表（值都是字符串）：读取元数据时不会执行脚本，否则
-- 版本不兼容的插件在"发现不兼容"之前就已经跑过一遍了。

PLUGIN = {
    name = "计算器",
    version = "1.0.0",
    author = "hemingtsai",
    license = "Apache-2.0",
    repository = "https://github.com/hemingtsai/touchery",
    min_touchery = "1.3.0",
    description = "四则运算 + 次方，回车复制结果",
}

-- 复制命令；测试或非 macOS 环境可用 TOUCHERY_CALC_COPY 覆盖（如 xclip / wl-copy）。
local COPY_CMD = os.getenv("TOUCHERY_CALC_COPY") or "pbcopy"

-- 允许的一元函数白名单。绝不能把 query 直接交给 loadstring 求值：那样
-- "> os.exit()" 之类会被当成 Lua 执行。
local FUNCS = {
  sqrt = math.sqrt,
  abs = math.abs,
  floor = math.floor,
  ceil = math.ceil,
  round = function(x)
    return math.floor(x + 0.5)
  end,
  sin = math.sin,
  cos = math.cos,
  tan = math.tan,
  log = math.log,
  log10 = math.log10,
  exp = math.exp,
  deg = function(x)
    return x * 180 / math.pi
  end,
  rad = function(x)
    return x * math.pi / 180
  end,
}

local CONSTS = {
  pi = math.pi,
  e = math.exp(1),
}

-- ---------------------------------------------------------------- 词法分析

local function tokenize(src)
  local tokens, i = {}, 1
  while i <= #src do
    local c = src:sub(i, i)
    if c:match("%s") then
      i = i + 1
    elseif c:match("[%d%.]") then
      local num = src:match("^%d+%.?%d*[eE][%+%-]?%d+", i) -- 1e3 / 1.5e-3
        or src:match("^%.%d+[eE][%+%-]?%d+", i) -- .5e3
        or src:match("^%d+%.?%d*", i) -- 123 / 1.5 / 5.
        or src:match("^%.%d+", i) -- .5
      if not num then
        return nil
      end
      tokens[#tokens + 1] = { k = "num", v = tonumber(num) }
      i = i + #num
    elseif c:match("[%a]") then
      local name = src:match("^%a[%w_]*", i)
      if CONSTS[name] then
        tokens[#tokens + 1] = { k = "num", v = CONSTS[name] }
      elseif FUNCS[name] then
        tokens[#tokens + 1] = { k = "func", v = FUNCS[name] }
      else
        return nil
      end
      i = i + #name
    elseif c:match("[%+%-%*/%%%^%(%)]") then
      tokens[#tokens + 1] = { k = c }
      i = i + 1
    else
      return nil
    end
  end
  if #tokens == 0 then
    return nil
  end
  return tokens
end

-- ---------------------------------------------------------------- 语法分析

local function peek(p)
  return p.t[p.i]
end

local function take(p)
  local token = p.t[p.i]
  p.i = p.i + 1
  return token
end

local function expect(p, k)
  local token = peek(p)
  if not token or token.k ~= k then
    error("缺少 " .. k, 0)
  end
  return take(p)
end

local parse_expr, parse_term, parse_unary, parse_power, parse_atom

function parse_atom(p)
  local token = take(p)
  if not token then
    error("表达式不完整", 0)
  end
  if token.k == "num" then
    return token.v
  end
  if token.k == "func" then
    expect(p, "(")
    local value = parse_expr(p)
    expect(p, ")")
    local ok, result = pcall(token.v, value)
    if not ok then
      error("函数计算失败", 0)
    end
    return result
  end
  if token.k == "(" then
    local value = parse_expr(p)
    expect(p, ")")
    return value
  end
  error("意外的符号", 0)
end

function parse_power(p)
  local base = parse_atom(p)
  if peek(p) and peek(p).k == "^" then
    take(p)
    -- 右结合：2^3^2 = 2^(3^2)
    return base ^ parse_unary(p)
  end
  return base
end

function parse_unary(p)
  local token = peek(p)
  if token and token.k == "-" then
    take(p)
    return -parse_unary(p)
  end
  if token and token.k == "+" then
    take(p)
    return parse_unary(p)
  end
  return parse_power(p)
end

function parse_term(p)
  local value = parse_unary(p)
  while true do
    local token = peek(p)
    if not token or (token.k ~= "*" and token.k ~= "/" and token.k ~= "%") then
      return value
    end
    take(p)
    local rhs = parse_unary(p)
    if token.k == "*" then
      value = value * rhs
    elseif token.k == "/" then
      if rhs == 0 then
        error("不能除以零", 0)
      end
      value = value / rhs
    else
      if rhs == 0 then
        error("不能对零取模", 0)
      end
      value = value % rhs
    end
  end
end

function parse_expr(p)
  local value = parse_term(p)
  while true do
    local token = peek(p)
    if not token or (token.k ~= "+" and token.k ~= "-") then
      return value
    end
    take(p)
    local rhs = parse_term(p)
    if token.k == "+" then
      value = value + rhs
    else
      value = value - rhs
    end
  end
end

-- 解析并求值；失败返回 nil 和原因。
local function evaluate(expr)
  local tokens = tokenize(expr)
  if not tokens then
    return nil, "无法解析"
  end
  local p = { t = tokens, i = 1 }
  local ok, value = pcall(parse_expr, p)
  if not ok then
    return nil, tostring(value)
  end
  if p.i <= #tokens then
    return nil, "多余的符号"
  end
  if type(value) ~= "number" then
    return nil, "结果不是数字"
  end
  if value ~= value then
    return nil, "结果无定义"
  end
  return value
end

-- ---------------------------------------------------------------- 格式化

local function format_number(x)
  if x == math.huge then
    return "∞"
  elseif x == -math.huge then
    return "-∞"
  end
  -- 整数不给小数点，其余去掉浮点噪声：0.1+0.2 显示 0.3 而不是 0.30000000000000004
  if x == math.floor(x) and math.abs(x) < 1e15 then
    return string.format("%d", x)
  end
  return string.format("%.10g", x)
end

-- ---------------------------------------------------------------- 剪贴板

local function copy(text)
  local ok = pcall(function()
    local pipe = io.popen(COPY_CMD, "w")
    if not pipe then
      error("无法启动 " .. COPY_CMD)
    end
    pipe:write(text)
    pipe:close()
  end)
  if not ok then
    -- 写日志到 stderr，方便从终端启动时排查
    io.stderr:write("[calc] 复制失败: " .. COPY_CMD .. "\n")
  end
  return ok
end

-- ---------------------------------------------------------------- 插件接口

function get_items(query)
  local expr = query or ""
  if expr == "" then
    return {
      {
        title = "计算器：输入算式后回车复制结果，如 1+2^3*4 或 sqrt(2)+pi",
        value = "",
        sub = false,
      },
    }
  end

  local value, err = evaluate(expr)
  if not value then
    -- 不是算式就不返回任何行，免得污染其他插件的结果
    return {}
  end

  local text = format_number(value)
  return {
    {
      title = expr .. " = " .. text,
      value = text,
      sub = false,
    },
  }
end

function run(value, query)
  if value and value ~= "" then
    copy(value)
  end
end

-- 本插件的所有条目都是 sub=false，这里只是为了满足插件契约而定义。
function run_sub(value, sub_query)
  if value and value ~= "" then
    copy(value)
  end
end
