return function(ctx)
  if ctx.event == "formatter.format" then
    return { action = { kind = "format", command = "document" } }
  elseif ctx.event == "formatter.check" then
    return { action = { kind = "format", command = "check" } }
  elseif ctx.event == "formatter.on_save" then
    return { action = { kind = "format", command = "on_save" } }
  end
  return {}
end
