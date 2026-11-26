# Этап 4. Сборка UltraLite

На Windows GNU `cargo build --locked --release` собирает desktop с feature
`ultralite` и игровой DLL без оверлея. Инструменты для Windows описаны
в [отчёте базовой сборки](ULTRALITE_BASELINE.md).

```powershell
cargo build --locked --release
cargo tree --locked -e features --target x86_64-pc-windows-gnu -p steam-rs -p steamclient
cargo test --locked --workspace --no-fail-fast
```

В графе EXE и DLL нет `store`, `video`, `voice`, `plugins`, `rhai`,
`str0m`, `opusic-sys`, `gilrs`, `global-hotkey`, `tray-icon`,
Chromium и WebView2. Реестр desktop содержит библиотеку. Вход, настройки
и загрузки используют окна UltraLite. При входе не создаются голосовой
обработчик и канал оверлея. Исключены фоновые запросы уровня профиля
и уведомлений. Основной игровой IPC остаётся частью клиентской сессии.
Состояние библиотеки хранится отдельно для каждого SteamID в настройках
UltraLite. Для хранения токенов используется Windows Credential Manager.

Полная конфигурация сохранена для сверки и синхронизации с upstream:

```powershell
cargo check --locked --no-default-features --features steam-rs/full,steamclient/overlay -p steam-rs -p steamclient --tests
```

Релиз x64 собрался за 8 мин 40 с. `cargo test --locked --workspace --no-fail-fast`
прошёл, 454 успешных теста, 0 ошибок, 4 пропущенных. Демонстрационное окно
открылось из готового EXE без входа.

| Файл | Базовая сборка, байты | UltraLite, байты |
| --- | ---: | ---: |
| `steam-rs.exe` x64 | 42 893 824 | 25 106 944 |
| `steamclient.dll` x64 | 8 242 688 | 7 354 368 |
| `steamclient.dll` i686 | — | 7 398 400 |

Сверка PE показала совпадение всех 41 имени экспорта и ординала x64
с 0.15.0. DLL i686 собрана с 32-битным MinGW, формат `PE i386`, 41 именованный
экспорт. Запуск игр и официальный fallback проверяются отдельно на этапе 6.
