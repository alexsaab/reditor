# Форматирование и панель плагинов

Соберите редактор и установите Lua-плагин для пользователя:

```sh
cargo build --release --locked
./target/release/reditor --install-plugin formatter --scope user
```

При открытом редакторе нажмите **F5**, чтобы перечитать плагины. Слева появится значок **FM**. Нажмите его мышью и выберите «Форматировать файл», «Проверить форматирование» или «Форматировать при сохранении». Те же команды доступны через **Ctrl+Shift+P** или палитру **Ctrl+G**; **Alt+Shift+F** форматирует текущий файл сразу. «Проверить форматирование» показывает изменения, не меняя документ. Обычное форматирование можно отменить через **Ctrl+Z**. Для команды нужен файл с именем и расширением; создайте новый файл через **Ctrl+S**.

| Файлы | Инструмент |
| --- | --- |
| `.rs` | `rustfmt`, с редакцией из ближайшего `Cargo.toml` |
| `.php`, `.phtml` | PHP CS Fixer, с настройками проекта `.php-cs-fixer.php` или `.php-cs-fixer.dist.php`; без них используется PSR-12 |
| `.js`, `.mjs`, `.cjs`, `.jsx`, `.ts`, `.mts`, `.cts`, `.tsx`, `.html`, `.htm`, `.css`, `.scss`, `.less`, `.json`, `.jsonc`, `.md`, `.markdown`, `.yaml`, `.yml`, `.vue`, `.graphql`, `.gql` | Prettier, с настройками проекта |

Установите `rustfmt` командой `rustup component add rustfmt`. Для Prettier из исходников проекта выполните `npm ci --prefix runtime/formatter`. Для пользовательской установки, работающей и с готовым бинарным файлом:

```sh
mkdir -p ~/.reditor/tools/prettier
cp runtime/formatter/package*.json ~/.reditor/tools/prettier/
npm ci --prefix ~/.reditor/tools/prettier
mkdir -p ~/.reditor/tools
curl -fsSL https://cs.symfony.com/download/php-cs-fixer-v3.phar -o ~/.reditor/tools/php-cs-fixer.phar
```

Для PHP нужен установленный `php` в `PATH`. PHP CS Fixer можно также поставить в проект через Composer (`vendor/bin/php-cs-fixer`). Prettier в проектном `node_modules/.bin` имеет приоритет над пользовательским. Любой инструмент можно указать явно в `~/.reditor/config.toml` или `<проект>/.reditor/config.toml`:

```toml
[plugin_settings.formatter]
on_save = true
rustfmt_binary = "/path/to/rustfmt"
prettier_binary = "/path/to/prettier"
php_cs_fixer_binary = "/path/to/php-cs-fixer.phar"
```

Команда в меню переключает `on_save` в проектной `.reditor/config.toml`; по умолчанию форматирование при сохранении выключено. При ошибке форматтера файл и буфер не заменяются. Форматирование ограничено файлами до 1 МиБ. Для другого плагина значок задаётся полем `icon` в `plugin.toml`; если его нет, панель показывает первые буквы имени. Колесо мыши прокручивает список значков; **EX** сверху переводит фокус в проводник, **ST** снизу открывает настройки.
