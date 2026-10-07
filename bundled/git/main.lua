return function(ctx)
 if ctx.event:match("^git%.") then
  return {action = {kind = "git", command = ctx.event}}
 end
 return {}
end
