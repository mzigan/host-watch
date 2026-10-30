# host-watch

Небольшой agentless монитор Linux-хостов. Один бинарник, OpenSSH, JSON state и SQLite. Реализован **MVP 2**: живой TUI с историей, RX/TX и optional CPU temperature.

## Сборка и запуск

Требуются Rust с поддержкой edition 2024, C compiler (bundled SQLite), Linux и `/usr/bin/ssh`. На удалённых хостах используются `/proc`, POSIX shell, awk, GNU df; systemctl опционален. Ничего устанавливать на серверы не требуется.

```sh
cargo build --release
cp config.example.toml config.toml
# Для локального запуска укажите data_dir в доступном вам каталоге.
./target/release/host-watch --config ./config.toml check
./target/release/host-watch --config ./config.toml daemon
./target/release/host-watch --config ./config.toml status
./target/release/host-watch --config ./config.toml status --verbose
./target/release/host-watch --config ./config.toml tui
```

`check` выполняет один проход и записывает state/history. `daemon` продолжает опрос до SIGINT/SIGTERM. `status` только читает state; права записи и работающий daemon не нужны. `tui` только читает state/SQLite и не подключается к серверам. Закрытие TUI не влияет на daemon. Успешное выполнение `check` означает завершённый проход, даже если некоторые хосты недоступны; состояние хостов смотрите в summary.

## Конфигурация

По умолчанию `/etc/host-watch/config.toml`. Полный пример — [config.example.toml](config.example.toml).

```toml
poll_interval_seconds = 60
history_days = 30
ssh_timeout_seconds = 10
max_parallel_hosts = 4
data_dir = "/var/lib/host-watch"

[thresholds]
cpu_warning = 90
temperature_warning = 80
temperature_critical = 90
memory_warning = 90
memory_critical = 98
disk_warning = 85
disk_critical = 95
load_warning_per_cpu = 1.5

[[hosts]]
name = "msk1"

[[hosts]]
name = "backup"
ssh_host = "backup-alias"
enabled = true
```

`name` — стабильный уникальный ключ state/history. `ssh_host` по умолчанию равен `name`. Изменение адреса в `~/.ssh/config` не меняет идентичность. Неизвестные параметры и некорректные пороги отклоняются.

OpenSSH читает конфигурацию пользователя, включая HostName, User, Port, IdentityFile, ProxyJump и SSH agent. Используется BatchMode, без запросов пароля, с обязательной проверкой host key. Сначала вручную проверьте `ssh alias` от имени пользователя сервиса и наполните known_hosts. Пароли не поддерживаются. SSH stderr не сохраняется, чтобы не переносить диагностические секреты в state; при ошибке проверьте подключение вручную. Таймаут ограничивает весь collector, включая подключение. `latency_ms` — полное время SSH-сеанса и сбора, а не отдельное измерение RTT подключения.

## Метрики и health

Один shell-блок за один SSH-сеанс: CPU count/usage, load 1/5/15, RAM, swap, локальные диски, uptime, процессы, failed systemd units, hostname, kernel, суммарные RX/TX всех non-loopback interfaces и optional CPU temperature. Файловые системы выбираются явным списком локальных типов; виртуальные и сетевые исключены. Root на overlay тоже исключается. Mount paths с пробелами поддерживаются, с переводами строк/табуляцией — нет.

Первый CPU snapshot — Unknown. CPU выше порога непрерывно пять минут даёт WARNING; ошибка опроса или длительный разрыв сбрасывает выдержку. RAM >90%, диск >85%, load на CPU >1.5 или failed units дают WARNING; RAM >98% или диск >95% — CRITICAL (пороги настраиваются). CPU temperature >80°C даёт WARNING, >90°C — CRITICAL. Неизвестная температура не влияет на health. Пороги сравниваются строго `>`.

Одна/две ошибки SSH/collector — UNKNOWN, три подряд — CRITICAL. Успех обнуляет ошибки и пересчитывает health. Недостаток данных не считается нулём. Если нет важных метрик (например systemd недоступен), состояние UNKNOWN, кроме уже обнаруженных WARNING/CRITICAL. `status` обнаруживает просроченный state, если daemon остановлен. Времена в verbose/state — Unix seconds UTC.

Пример summary после двух успешных опросов:

```text
HOSTS: 2
OK: 1
WARNING: 0
CRITICAL: 1
UNKNOWN: 0

backup: CRITICAL
  disk[/]=97%
```

## State и история

В `data_dir`: `state.json`, `history.db`, `writer.lock`. Единственный writer (daemon или check) защищён flock. JSON пишется через temp → fsync → rename → fsync каталога. Не редактируйте файлы параллельно с writer. Повреждённый JSON приводит к явной ошибке.

SQLite WAL: таблица `snapshots(timestamp, host, health, metrics_json, latency_ms, stale, rx_bytes_per_sec, tx_bytes_per_sec, cpu_temperature_c, max_disk_used_percent)`, ключ `(host,timestamp)`. JSON содержит все полученные метрики и диски, отсутствующие поля — null. При ошибке метрики/latency в history — NULL, stale=1; прежние метрики остаются только в state. Retention выполняется в каждой транзакции вставки. State и SQLite не образуют общую транзакцию: при аварии между записями последний snapshot истории может отсутствовать. Несколько check в одну секунду заменяют строку той же секунды. Удаление/отключение хоста убирает его из текущего state после следующего прохода, история живёт до retention.

## Systemd

Unit: [packaging/host-watch.service](packaging/host-watch.service). Выберите в `User=` существующего непривилегированного пользователя с настроенным SSH. Имя `host-watch` в примере — placeholder; аккаунт автоматически не создаётся. Сервис использует HOME и SSH config этого пользователя, а не пользователя, вызвавшего sudo. Интерактивный SSH agent обычно недоступен systemd: используйте подходящий IdentityFile или явно настройте SSH_AUTH_SOCK.

```sh
sudo install -m 0755 target/release/host-watch /usr/local/bin/host-watch
sudo install -d -m 0755 /etc/host-watch
sudo install -m 0644 config.example.toml /etc/host-watch/config.toml
# Отредактируйте config.toml и User= в unit перед установкой.
sudo install -m 0644 packaging/host-watch.service /etc/systemd/system/host-watch.service
sudo systemctl daemon-reload
sudo systemctl enable --now host-watch.service
journalctl -u host-watch.service
```

Systemd создаёт `/var/lib/host-watch` с владельцем User и mode 0700. При ручном запуске создайте каталог с подходящим владельцем/правами либо задайте свой data_dir. Для другого data_dir скорректируйте ограничения записи unit (`ReadWritePaths=`). SSH-конфигурация должна быть полностью подготовлена до запуска.

## TUI, сеть и температура

TUI использует ratatui/crossterm. Таблица сверху: host, health, CPU, RAM, max disk, load1, SSH round trip. Ниже — детали с mountpoints и графики CPU/RAM/load/SSH/disk/RX/TX. Обновление раз в секунду. Для истории не нужен отдельный процесс, но новые данные поступают только от daemon/check.

Клавиши:

| Клавиша | Действие |
| --- | --- |
| ↑ / ↓ | Выбрать хост |
| Enter | Фокус деталей |
| Tab | Таблица → детали → графики |
| ← / → | Переключить набор графиков |
| PgUp / PgDn | Прокрутить детали |
| 1 / 2 / 3 | Диапазон 1h / 24h / 7d |
| r | Перечитать state/history |
| q / Esc / Ctrl+C | Выйти |

На широком терминале детали и графики видны одновременно (от 110×24). Число одновременно видимых графиков зависит от высоты: 1/2/4/7. На узком терминале по умолчанию показаны графики, Enter открывает детали, Tab переключает нижнюю панель; минимальный размер — 55×12. Выбранный хост выделен; цвета дополняют текст health. Unknown — `-`, а не ноль.

`STALE` означает ошибку последнего опроса, отсутствие успешного опроса либо превышение `2 × poll_interval_seconds + ssh_timeout_seconds`. Значения в этой строке и деталях — последние известные. Если state временно недоступен, остаётся предыдущий snapshot с отметкой stale и сообщением об ошибке. При проблемах SQLite текущие данные доступны, графики показывают `history unavailable`. TUI открывает БД read-only, без создания и миграции.

Запрос истории ограничен выбранным host и диапазоном. Строки обрабатываются потоково в ограниченное число временных корзин под ширину терминала: average для CPU/RAM/load/SSH/RX/TX, maximum для диска. Соседние измерения соединяются линиями даже при пустых промежуточных корзинах отрисовки. Ошибки/отсутствующие метрики и интервалы между измерениями больше `2 × poll_interval_seconds + ssh_timeout_seconds` дают разрывы. Графики подписаны последним значением, min/avg/max и временными отметками. Avg — среднее всех известных измерений в выбранном диапазоне с учётом числа samples в каждой корзине; Unknown пропускается. Жёлтая пунктирная линия на CPU/RAM/Disk обозначает warning threshold из конфига (CPU — уровень, выдержка по времени по-прежнему применяется только в health); RX/TX автоматически используют B/s, KiB/s или MiB/s.

RX/TX берутся из `/proc/net/dev`, исключая `lo`. Включены также виртуальные non-loopback интерфейсы; при bridge/veth возможен двойной учёт, это не счётчик трафика провайдера. Rate = delta bytes / delta remote uptime. Первый snapshot, сброс счётчика или reboot дают Unknown. Network не влияет на health.

Температура берётся из `sensors -u`, если команда уже установлена: предпочтительно `Package id`, затем `Tdie`, затем `Tctl`; среди нескольких CPU packages выбирается максимум. Generic `temp1` и неразумные значения игнорируются. Отсутствие sensors или узнаваемого CPU label — Unknown, без ошибки и без установки пакетов.

При первом запуске нового daemon/check недостающие SQLite-колонки добавляются транзакционно; прежние строки сохраняются. В новых колонках прежних строк — NULL. Старые CPU/RAM/load/disk доступны из `metrics_json` без backfill. Старые config/state остаются совместимыми. Устанавливая новую версию, перезапустите daemon: TUI самостоятельно не обновляет collector.

## Проверки и следующие этапы

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build --release
```

Автоматические тесты используют fixtures, временные SQLite/state, TestBackend Ratatui, локальный collector и SSH с `ProxyCommand=false`, без подключения к серверам. Проверены старый config/state/DB, миграция, network delta/reset/reboot, temperature, диапазоны, downsampling и fallback при недоступных данных.

Позже: ограниченная проверка kernel/journal, уведомления. Нет агентов, web UI, generic rule engine или внешней monitoring platform.
