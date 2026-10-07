return function(ctx)
    local _, lines = ctx.text:gsub("\n", "\n")
    return {
        panel = {
            title = ctx.settings.title,
            content = "API: " .. tostring(ctx.apiVersion)
                .. "\nUTF-8 bytes: " .. tostring(#ctx.text)
                .. "\nLines: " .. tostring(lines + 1)
                .. "\nFile: " .. tostring(ctx.path)
        }
    }
end
