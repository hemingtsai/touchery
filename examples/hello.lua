-- touchery 示例插件
-- 安装：复制到 ~/Library/Application Support/touchery/plugins/hello.lua
-- 使用：唤起启动器后输入 "> 关键词" 路由到本插件
--
-- 契约：
--   PLUGIN                     可选：插件自述（名称/版本/作者/许可证/仓库/说明）
--   get_items(query)          -> { { title, value, sub }, ... }  sub=true 表示需要二级输入
--   run(value, query)         -> 一级回车（sub=false 的条目）
--   run_sub(value, sub_query) -> 二级输入回车后调用

PLUGIN = {
    name = "示例插件",
    version = "1.0.0",
    author = "hemingtsai",
    license = "Apache-2.0",
    repository = "https://github.com/hemingtsai/touchery",
    description = "演示插件契约：问候、回声与二级输入",
}

local LOG = os.getenv("HOME") .. "/Library/Application Support/touchery/plugin.log"

local function log(line)
    local f = io.open(LOG, "a")
    if f then
        f:write(line .. "\n")
        f:close()
    end
end

function get_items(query)
    log("get_items: [" .. tostring(query) .. "]")
    if query == "" then
        return {
            { title = "问候", value = "hello", sub = false },
            { title = "回声（需二级输入）", value = "echo", sub = true },
        }
    end
    return {
        { title = "一级执行: " .. query, value = query, sub = false },
        { title = "二级处理: " .. query, value = query, sub = true },
    }
end

function run(value, query)
    log("run: value=[" .. tostring(value) .. "] query=[" .. tostring(query) .. "]")
end

function run_sub(value, sub_query)
    log("run_sub: value=[" .. tostring(value) .. "] sub_query=[" .. tostring(sub_query) .. "]")
end
