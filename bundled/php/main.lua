local labels = {
 ru = {"PHP: структура", "Классы и функции не найдены", "Шаблон предназначен для пустого файла"},
 en = {"PHP outline", "No classes or functions found", "Template requires an empty file"},
 de = {"PHP-Struktur", "Keine Klassen oder Funktionen gefunden", "Die Vorlage benötigt eine leere Datei"},
 es = {"Estructura PHP", "No se encontraron clases ni funciones", "La plantilla necesita un archivo vacío"}
}
return function(ctx)
 local label = labels[ctx.language] or labels.en
 if ctx.event == "php.lint" or ctx.event == "php.run" or ctx.event == "php.server" then
  return {action = {kind = "php", command = ctx.event}}
 elseif ctx.event == "php.template" then
  if ctx.text:match("%S") then return {message = label[3]} end
  return {text = "<?php\ndeclare(strict_types=1);\n\n// PHP\n\n"}
 elseif ctx.event == "php.outline" then
  local lines, row = {}, 0
  for line in (ctx.text .. "\n"):gmatch("([^\n]*)\n") do
   row = row + 1
   local kind, name = line:match("%f[%a](class)%s+([%a_][%w_]*)")
   if not name then kind, name = line:match("%f[%a](function)%s+&?%s*([%a_][%w_]*)%s*%(") end
   if not name then kind, name = line:match("%f[%a](interface)%s+([%a_][%w_]*)") end
   if not name then kind, name = line:match("%f[%a](trait)%s+([%a_][%w_]*)") end
   if not name then kind, name = line:match("%f[%a](enum)%s+([%a_][%w_]*)") end
   if name then lines[#lines+1] = row .. ": " .. kind .. " " .. name end
  end
  return {panel = {title = label[1], content = #lines > 0 and table.concat(lines, "\n") or label[2]}}
 end
 return {}
end
