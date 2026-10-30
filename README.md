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

## Установка на другом компьютере (Linux + systemd)

Устанавливайте host-watch на компьютер, с которого будете мониторить серверы. На самих серверах устанавливать host-watch не нужно. Команды ниже выполняются обычным пользователем с sudo; сервис будет работать от этого пользователя и читать его `~/.ssh/config`.

### 1. Получить исходники и собрать

Для Debian/Ubuntu установите инструменты сборки и OpenSSH:

```sh
sudo apt update
sudo apt install -y git build-essential pkg-config openssh-client
```

Нужен современный stable Rust с Cargo; пакет Rust из старого дистрибутива может не поддерживать зависимости из Cargo.lock. Если Rust ещё не установлен, установите через rustup:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/host-watch-rustup-init.sh
sh /tmp/host-watch-rustup-init.sh
. "$HOME/.cargo/env"
```

Для скачивания установщика нужны `curl` и CA certificates (`sudo apt install curl ca-certificates`, если отсутствуют). Если rustup уже установлен, достаточно `rustup update stable`.

```sh
git clone https://github.com/mzigan/host-watch.git
cd host-watch
cargo +stable build --release --locked
```

Сборка на целевом компьютере исключает несовместимость готового бинарника по архитектуре или glibc. SQLite собирается вместе с проектом; отдельный сервер БД не нужен.

### 2. Подготовить SSH

Создайте собственные aliases в `~/.ssh/config` и настройте соответствующие ключи. Названия в конфиге host-watch должны совпадать с aliases. Например:

```sshconfig
Host app-server
    HostName 192.0.2.10
    User monitor
    IdentityFile ~/.ssh/monitor_app
    IdentitiesOnly yes

Host backup-server
    HostName 192.0.2.20
    User monitor
    IdentityFile ~/.ssh/monitor_backup
    IdentitiesOnly yes
    # ProxyJump app-server
```

Адреса, пользователи и пути в примере нужно заменить своими. Сначала подключитесь обычным `ssh app-server`, проверьте отпечаток сервера и добавьте его в known_hosts. Затем убедитесь, что оба подключения работают без интерактивных запросов и без agent:

```sh
env -u SSH_AUTH_SOCK ssh -T -o BatchMode=yes -o StrictHostKeyChecking=yes app-server 'printf "SSH OK\n"'
env -u SSH_AUTH_SOCK ssh -T -o BatchMode=yes -o StrictHostKeyChecking=yes backup-server 'printf "SSH OK\n"'
```

Ожидается `SSH OK`. Проверьте каждый alias, включая промежуточные хосты ProxyJump. Сервис не наследует SSH_AUTH_SOCK интерактивного терминала. Если ключ защищён passphrase, отдельно настройте доступ сервиса к постоянно работающему разблокированному agent; одного IdentityFile недостаточно. После перезагрузки agent также должен быть доступен до опроса.

Если возникает `Permission denied`, проверьте User и IdentityFile. Для конечного хоста за ProxyJump может требоваться другой ключ, чем для jump host. Сопоставить отпечатки можно через `ssh-keygen -lf ~/.ssh/имя_ключа.pub` локально и `ssh-keygen -lf ~/.ssh/authorized_keys` на сервере. Для диагностики используйте `ssh -v -o BatchMode=yes alias`; не отключайте проверку host key.

SSH-ключи, личный config.toml и known_hosts не входят в репозиторий; доступ на новом компьютере нужно подготовить отдельно. Пароли в config.toml не поддерживаются.

### 3. Установить бинарник и конфиг

```sh
sudo install -m 0755 target/release/host-watch /usr/local/bin/host-watch
sudo install -d -m 0755 /etc/host-watch
sudo install -m 0644 config.example.toml /etc/host-watch/config.toml
sudo nano /etc/host-watch/config.toml
```

Замените список hosts своими aliases, например `app-server` и `backup-server`. Для приведённого systemd unit оставьте:

```toml
data_dir = "/var/lib/host-watch"
```

Этот путь отличается от каталога данных для ручного запуска в домашней директории. Новый компьютер начнёт историю с нуля, если не перенести существующие данные (см. ниже).

### 4. Установить systemd unit

Из каталога проекта, от обычного пользователя, настроившего SSH:

```sh
watch_user="$(id -un)"
sudo install -m 0644 packaging/host-watch.service /etc/systemd/system/host-watch.service
sudo sed -i "s/^User=.*/User=$watch_user/" /etc/systemd/system/host-watch.service
sudo systemctl daemon-reload
sudo systemctl enable --now host-watch.service
```

`User=host-watch` в исходном [unit-файле](packaging/host-watch.service) — placeholder; приведённая команда заменяет его текущим пользователем. Сервис использует HOME и SSH config этого пользователя. Systemd создаёт `/var/lib/host-watch` с владельцем User и mode 0700. Для другого data_dir потребуется отдельно настроить разрешённый путь записи unit (`ReadWritePaths=`).

### 5. Проверить и открыть TUI

```sh
systemctl status host-watch.service --no-pager
journalctl -u host-watch.service -n 50 --no-pager
host-watch status --verbose
host-watch tui
```

`status` и `tui` запускайте от того же пользователя, без sudo; по умолчанию они читают `/etc/host-watch/config.toml`. Для CPU/RX/TX нужны два успешных опроса, поэтому после первого запуска подождите примерно минуту. TUI не опрашивает серверы самостоятельно. Если daemon не пишет state/history, TUI покажет Unknown/stale или history unavailable.

Сервис продолжит работать после закрытия терминала и запустится после перезагрузки. Закрытие TUI не останавливает daemon. При `another daemon/check is already writing` остановите второй writer; не запускайте check параллельно с сервисом в одном data_dir.

### Перенос существующей истории

Перенос необязателен. Остановите все writers старого data_dir (systemd-сервис через `sudo systemctl stop host-watch`, ручной daemon через Ctrl+C). Затем скопируйте **весь каталог данных**, включая state.json, history.db и имеющиеся history.db-wal/history.db-shm. Копировать только history.db при работающем daemon нельзя: последние записи могут находиться в WAL.

На новом компьютере перед восстановлением остановите host-watch. Поместите файлы в `/var/lib/host-watch`, назначьте владельцем пользователя из User= и установите mode 0700 для каталога. Сохраните конфигурационные имена hosts — это ключи истории. После переноса запустите сервис. SSH-конфигурация и ключи настраиваются отдельно.

### Обновление установленной версии

В каталоге клонированного проекта:

```sh
git pull --ff-only
cargo +stable build --release --locked
sudo systemctl stop host-watch.service
sudo install -m 0755 target/release/host-watch /usr/local/bin/host-watch
sudo systemctl start host-watch.service
```

Перезапустите открытые TUI. Конфиг и data_dir при обновлении не заменяются; миграция SQLite выполняется автоматически новым daemon/check. После изменения `/etc/host-watch/config.toml` также перезапустите daemon; после изменения unit выполните `sudo systemctl daemon-reload` и restart.

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

На большом терминале (от 150×30) список серверов занимает отдельную левую колонку на всю высоту рабочей области; детали выбранного хоста находятся в средней колонке, графики — справа. Список прокручивается при выборе стрелками; в узкой боковой колонке остаются HOST/STATE/CPU/RAM, остальные значения доступны в деталях. При размере от 110×24 список расположен сверху, детали и графики видны одновременно. Число одновременно видимых графиков зависит от высоты: 1/2/4/7. На узком терминале по умолчанию показаны графики, Enter открывает детали, Tab переключает нижнюю панель; минимальный размер — 55×12. Выбранный хост выделен; цвета дополняют текст health. Unknown — `-`, а не ноль.

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
