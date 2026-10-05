# Прямые зависимости

Исходный код MakePort распространяется под GPL-3.0-only.

| Компонент | Использование | Лицензия выбранного варианта |
|---|---|---|
| Slint / slint-build 1.17.1 | Интерфейс и его компилятор | GPL-3.0-only |
| serde 1.0.228 | Сериализация | MIT OR Apache-2.0 |
| toml 0.8.23 | TOML | MIT OR Apache-2.0 |
| directories 6.0.0 | Папки ОС | MIT OR Apache-2.0 |
| tempfile 3.23.0 | Атомарная запись | MIT OR Apache-2.0 |
| fs2 0.4.3 | Блокировка экземпляра | MIT OR Apache-2.0 |
| winresource 0.1.31 | Иконка и сведения Windows EXE при сборке | MIT |
| zbus 5.19.0 | Проверка доступности системного трея на Linux | MIT |

OpenSSH вызывается как системная программа и не включён в архив проекта.
Все файлы в remote/ принадлежат этому проекту и не заимствованы из OpenSSH.

Транзитивные зависимости и их лицензии следует учитывать по фактическому
Cargo.lock после сборки. Для распространения исполняемых сборок сохраните
соответствующие исходники, Cargo.lock и уведомления зависимостей.
Этот файл не является автоматически сформированным полным реестром зависимостей.

Основная документация, использованная при реализации:

- https://docs.rs/crate/slint/1.17.1/features
- https://docs.slint.dev/latest/docs/slint/reference/std-widgets/globals/palette/
- https://docs.slint.dev/latest/docs/slint/reference/std-widgets/views/textedit/
- https://man.openbsd.org/ssh
- https://man.openbsd.org/ssh_config
- https://doc.rust-lang.org/cargo/getting-started/installation.html
- https://github.com/PowerShell/Win32-OpenSSH
