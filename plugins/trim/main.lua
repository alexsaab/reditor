return function(ctx)
    if ctx.event ~= "trim" and ctx.event ~= "before_save" then return {} end
    local text = ctx.text:gsub("[ \t]+(\r?\n)", "%1"):gsub("[ \t]+$", "")
    return { text = text }
end
