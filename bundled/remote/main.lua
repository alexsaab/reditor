return function(ctx)
 if ctx.event:match("^remote%.") then
  return {action = {kind = "remote", command = ctx.event}}
 end
 return {}
end
