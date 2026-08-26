-- Touchery 示例主题
-- 安装：复制到 ~/Library/Application Support/touchery/themes/<名字>.lua
-- 规则：
--   * 文件必须 return 一个 table，包含 light / dark 两个子表（可只定义其一）
--   * 颜色格式：#rgb、#rrggbb 或 #rrggbbaa（最后两位是透明度）
--   * 未定义的颜色自动回退到内置主题对应亮/暗色的值
--   * 系统切换深浅色外观时自动套用对应的子表

return {
    name = "示例：暖色纸感",

    light = {
        card_bg      = "#faf6efF2",
        card_border  = "#d8cfc099",
        panel_bg     = "#f4efe6FC",
        row_bg       = "#0000000D",
        input_bg     = "#FFFFFFD9",
        input_border = "#b3a89455",
        text_primary = "#2b2620FF",
        text_secondary = "#7a7163CC",
        accent_info  = "#b06f2fFF",
        accent_ok    = "#3e8e41FF",
        accent_error = "#c2452dFF",
    },

    dark = {
        card_bg      = "#241f1aF0",
        card_border  = "#5a4c3c80",
        panel_bg     = "#28221cFC",
        row_bg       = "#ffffff08",
        input_bg     = "#ffffff14",
        input_border = "#ffffff26",
        -- 未定义的 text_* 与 accent_* 回退到内置暗色值
    },
}
