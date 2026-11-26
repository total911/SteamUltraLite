# Этап 1. Базовая сборка SteamInRust 0.15.0

Исходники для сравнения взяты из upstream-тега `v0.15.0`, SHA
`abe8275afb47e002dea06216b30502d1613970c2`. Ветка `ultralite` началась с
`458c4fe59929e83a3f06be1c73cd60c0341d1488` и содержит более позднее изменение
запуска второго экземпляра. Для замеров используются исходники тега.

## Windows GNU

- Windows 11 x64, сборка ОС `26200`.
- Rust и Cargo `1.100.0`, цель `x86_64-pc-windows-gnu`.
- MSYS2 MINGW64. GCC `16.2.0`, CMake `4.5.1`, Ninja `1.13.2`.

В терминале MSYS2 установите пакеты:

```sh
pacman -S --needed mingw-w64-x86_64-gcc mingw-w64-x86_64-binutils mingw-w64-x86_64-cmake mingw-w64-x86_64-ninja
```

В PowerShell добавьте каталог `mingw64/bin` в `PATH` текущего процесса.
Явно задайте toolchain, как в upstream CI:

```powershell
rustup toolchain install stable-x86_64-pc-windows-gnu --profile minimal
$env:RUSTUP_TOOLCHAIN = 'stable-x86_64-pc-windows-gnu'
$env:CMAKE_GENERATOR = 'Ninja'
```

## Lockfile релизного тега

Команда `cargo build --locked --release` на точном checkout тега завершилась
с кодом `1` и ошибкой `cannot update the lock file ... because --locked was passed`.
В теге `Cargo.toml` задаёт `0.15.0`, а 19 пакетов workspace в `Cargo.lock` ещё
помечены как `0.14.0`.

Для повторной сборки использована отдельная копия тега с `Cargo.lock` из
upstream-коммита `401bf665b8d8ba406b475b3fcee74a7f29616e9e`. Сверка показала
единственный изменённый файл и только 19 замен версий внутренних пакетов.
Исходники, версии внешних зависимостей и их контрольные суммы взяты из тега.

## Результаты после подстановки lockfile

- `cargo build --locked --release` — код `0`, время сборки `15m 54s`.
- `cargo test --workspace` — первый запуск остановился на `steamclient::p2p::tests::joins_an_official_host`. Тест не получил один из двух ожидаемых пакетов. Отдельный прогон теста прошёл, полный повторный прогон — `582` успешных, `0` ошибок, `5` пропущенных. Код теста и DLL не менялись.
- `cargo run -- --demo --no-local --separate --offscreen --page=library` — открылось окно `SteamInRust`, Windows подтвердила отклик. Такой же запуск готового релиза прошёл. Вход и локальная библиотека не использовались. После проверки debug-процесс завершён принудительно, поскольку закрытие окна не завершило его за 6 секунд.

| Файл | Байты | SHA-256 |
| --- | ---: | --- |
| `steam-rs.exe` | 42 893 824 | `dc23643e4db6b620b7c83cc7755f7df9b399bde72b6e1b24e17de2895f1606ba` |
| `steamclient.dll` | 8 242 688 | `d7d86dc6a4a931d8fa3c7a54d32440ee09d4032e55e443174b51a1aba93fd264` |
