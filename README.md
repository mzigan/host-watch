# host-watch

Небольшой agentless монитор Linux-хостов. Один бинарник, OpenSSH, JSON state и SQLite. Реализован **MVP 1**; TUI пока не реализован.

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
```

`check` выполняет один проход и записывает state/history. `daemon` продолжает опрос до SIGINT/SIGTERM. `status` только читает state; права записи и работающий daemon не нужны. `tui` сообщает, что эта функция отложена до MVP 2. Успешное выполнение `check` означает завершённый проход, даже если некоторые хосты недоступны; состояние хостов смотрите в summary.

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

Один shell-блок за один SSH-сеанс: CPU count/usage, load 1/5/15, RAM, swap, локальные диски, uptime, процессы, failed systemd units, hostname, kernel. Файловые системы выбираются явным списком локальных типов; виртуальные и сетевые исключены. Root на overlay тоже исключается. Mount paths с пробелами поддерживаются, с переводами строк/табуляцией — нет.

Первый CPU snapshot — Unknown. CPU выше порога непрерывно пять минут даёт WARNING; ошибка опроса или длительный разрыв сбрасывает выдержку. RAM >90%, диск >85%, load на CPU >1.5 или failed units дают WARNING; RAM >98% или диск >95% — CRITICAL (пороги настраиваются). Пороги сравниваются строго `>`.

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

SQLite WAL: таблица `snapshots(timestamp, host, health, metrics_json, latency_ms, stale)`, ключ `(host,timestamp)`. JSON содержит все полученные метрики и диски, отсутствующие поля — null. При ошибке метрики/latency в history — NULL, stale=1; прежние метрики остаются только в state. Retention выполняется в каждой транзакции вставки. State и SQLite не образуют общую транзакцию: при аварии между записями последний snapshot истории может отсутствовать. Несколько check в одну секунду заменяют строку той же секунды. Удаление/отключение хоста убирает его из текущего state после следующего прохода, история живёт до retention.

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

## Проверки и следующие этапы

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build --release
```

Тесты используют fixtures, временные SQLite/state, локальный collector и SSH с `ProxyCommand=false`, без подключения к серверам.

MVP 2: ratatui/crossterm TUI, графики 1h/24h/7d, RX/TX throughput, температуры. Позже: ограниченная проверка kernel/journal, уведомления. Нет агентов, web UI, generic rule engine или внешней monitoring platform.
