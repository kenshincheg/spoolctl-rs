# SpoolCtl

Portable Rust-утилита для службы печати Windows (**Spooler**): статус, перезапуск и очистка очереди, когда служба зависает.

Управление — через Win32 Service Control Manager и файлы очереди, без PowerShell.

## Совместимость

| Система | Где в архиве | Режим |
|---------|--------------|--------|
| **Windows 10 / 11 x64** | корень: `SpoolCtl.exe` | GUI + CLI |
| **Windows 7 x64** | папка **`Windows7-CLI\`** | только CLI |

Подробнее: [`docs/WIN7.md`](docs/WIN7.md).

## Статус

**0.1.15** (стабильный): автозапуск elevated, трей, сторож, компактный UI с иконками действий.

## Сборка portable

```bat
tools\build-portable.cmd
```

Кэш сборки (`target`) лежит на локальном диске: `E:\cargo-target\spoolctl-rs` (см. `.cargo/config.toml`).

Результат:

- `dist\stage\SpoolCtl-0.1.15\` — готовая папка (Win10/11 + `Windows7-CLI`)
- `dist\SpoolCtl-0.1.15-win64.zip`
- `dist\SpoolCtl-0.1.15-win64.zip.sha256`

Заметки релиза: [`docs/RELEASE_NOTES.md`](docs/RELEASE_NOTES.md).

## CLI

```bat
SpoolCtl.exe status
SpoolCtl.exe stop
SpoolCtl.exe start
SpoolCtl.exe restart
SpoolCtl.exe clear-queue
SpoolCtl.exe fix
```

`stop` / `start` / `restart` / `clear-queue` / `fix` требуют прав администратора.

Журнал (без секретов): `logs\spoolctl.log` рядом с EXE или `%LOCALAPPDATA%\SpoolCtl\logs`.

План: [`docs/PLAN.md`](docs/PLAN.md).
