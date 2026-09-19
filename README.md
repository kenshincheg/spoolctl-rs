# SpoolCtl

Portable Rust-утилита для службы печати Windows (**Spooler**): статус, перезапуск и очистка очереди, когда служба зависает.

Программа создавалась для удобства и автоматизации типичной ситуации: служба печати Spooler иногда «зависает», очередь не двигается, документы не печатаются. SpoolCtl помогает быстро увидеть состояние, безопасно остановить/запустить службу, очистить застрявшую очередь и при желании следить за зависанием из трея — без ручной возни с `services.msc` и папкой `spool\PRINTERS`.

Управление — через Win32 Service Control Manager и файлы очереди, без PowerShell.

## Участие в разработке

Буду рад, если кто-то присоединится к разработке SpoolCtl: улучшение интерфейса, удобство использования, багфиксы, документация и любые другие доработки. Pull request’ы и идеи в Issues приветствуются.

## Совместимость

| Система | Где в архиве | Режим |
|---------|--------------|--------|
| **Windows 10 / 11 x64** | корень: `SpoolCtl.exe` | GUI + CLI |
| **Windows 7 x64** | папка **`Windows7-CLI\`** | только CLI |

Подробнее: [`docs/WIN7.md`](docs/WIN7.md).

## Статус

**0.2.0** (разработка): удалённое управление через SCM — см. [`docs/REMOTE.md`](docs/REMOTE.md).  
Стабильная линия **0.1.15** без изменений в `dist\stage\`.

## Сборка

Стабильная 0.1.x:

```bat
tools\build-portable.cmd
```

Линия 0.2 (отдельные папки, не затирает 0.1.15):

```bat
tools\build-portable-0.2.cmd
```

- target: `E:\cargo-target\spoolctl-rs-0.2`
- stage: `dist\stage-0.2\SpoolCtl-0.2.0\`
- zip: `dist\SpoolCtl-0.2.0-win64.zip`

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
