return function(ctx)
  local language = ctx.event:match("^proofreader%.([a-z]+)$")
  if language then
    return { action = { kind = "proofread", command = language } }
  end
  return {}
end
