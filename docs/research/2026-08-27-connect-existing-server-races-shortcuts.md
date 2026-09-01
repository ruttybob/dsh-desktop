# dsh-desktop: подключение к внешнему серверу, конфликты параллельного доступа, горячие клавиши

Дата: 2026-08-27. Все утверждения опираются на исходный код `dsh-desktop` и `deepseek-harness` (пути/строки указаны инлайном) либо официальную документацию Tauri.

---

## Q1. Может ли приложение подключиться к уже запущенному серверу `dsh web` вместо собственного sidecar?

### Что есть сегодня: единственный путь — spawn + разбор stdout

1. `lib.rs` в `setup` безусловно создаёт `HostManager::spawn` для окна `main` (src-tauri/src/lib.rs:13-20). Альтернативных веток нет.
2. `HostManager::spawn` собирает команду `node main.mjs`, задаёт `DSH_DESKTOP_PORT=0` (OS-выбранный порт), пайпы stdout/stderr и отдельную процессную группу на Unix (host.rs:58-82).
3. Ридер stdout пересылает строки в лог и ищет маркер готовности: `parse_url_line` принимает только строку с префиксом `dsh web: http://127.0.0.1:` и извлекает порт; прочие строки (включая LAN-URL `http://0.0.0.0:…`) отбрасываются, тесты это фиксируют (host.rs:339-350, 463-480). Найденный URL идёт в `window.navigate(url)` (host.rs:110-120).
4. `main.mjs` переписывает `process.argv` на `['node', <bin>, 'web', '--port', $DSH_DESKTOP_PORT ?? '0', '--no-open']` и добавляет по одному `--trusted-host` на элемент `DSH_DESKTOP_TRUSTED_HOSTS` (host/main.mjs:28-35).

Швов «не спавнить sidecar» нет: ни env-переменной, ни config-ключа, ни CLI-аргумента Tauri-приложения для attach-режима в коде не существует (lib.rs целиком — 30 строк; host.rs — едиственный pathway).

### Есть ли со стороны сервера поддержка «подключения» к чужому URL?

Нет. Веб-рантайм всегда сам биндит сокет: `WebServer` активацией вызывает `this.server.listen(this.config.port, this.config.host, …)` (packages/host/webserver/src/index.ts:233-236); README пакета описывает режим «listen on activation», а EADDRINUSE отвергает композицию (packages/host/webserver/README.md, раздел Known Limitations: «A listen failure … throws out of activation»). Flags CLI ограничены `--host/--no-open/--port/--trusted-host` (packages/bundle/web-app/src/startup.ts:51-54) — флага вроде `--connect-to` не существует. Т.е. вопрос стоит только о том, чтобы WebView шёл на *чужой* уже слушающий процесс.

### Как trust взаимодействует с внешним host:port

Ограждение `/api` (browser-trust fence) обязывает каждый запрос предъявлять `Host`, который является loopback-авторитетом ИЛИ совпадает с элементом `trustedHosts` (`isTrustedApiRequest`, packages/client/connection/src/api-request-trust.ts:96-110). Значит:

- указание WebView на `http://127.0.0.1:<порт чужого процесса>` проходит fence **без** `--trusted-host` (loopback);
- не-loopback имя/адрес (например LAN IP терминального сервера) потребует, чтобы *этот* сервер был запущен с соответствующим `--trusted-host` — докидывать его через `DSH_DESKTOP_TRUSTED_HOSTS` бессмысленно, т.к. этот механизм конфигурирует собственный sidecar (host/main.mjs:29-33), а fence проверяется на стороне того хоста, к которому идёт запрос (README: «the CLI's `--trusted-host` flag declare named authorities», packages/client/connection/README.md, строка про node half).

`--host 0.0.0.0` намеренно запрещён на уровне парсера (startup.ts:74-76) и вебсервер принимает только `127.0.0.1`/`0.0.0.0` (README webserver: «host accepts only 127.0.0.1 (default posture) and 0.0.0.0»).

### Минимальная реализация attach-режима

Изменения только в shell-репозитории:

1. `src-tauri/src/lib.rs` / `host.rs`: новая ветка в `setup` — если задан, скажем, `DSH_DESKTOP_ATTACH_URL`, пропустить `HostManager::spawn` и сразу вызвать `window.navigate(Url::parse(...))` (тот же вызов, что в host.rs:114). Логика `stop()` на Exit становится no-op (child = None уже обрабатывается, host.rs:129-140).
2. Optionally: проверить схему/хост URL перед навигацией (сегодня единственная «валидация» — префикс `http://127.0.0.1:` в `parse_url_line`, host.rs:340-344).
3. Ни harness, ни `main.mjs` менять не нужно при loopback-цели; для не-loopback — документировать требование запуска терминального сервера с `--trusted-host`.

Оценка выполнимости: высокая (~десятки строк Rust в одном файле). Главные вопросы — UX (куда пользователь вводит URL) и деградация: сегодня отсутствие child просто оставляет окно на splash с логами (host.rs:26-40), attach-режим должен делать то же при недостижимом URL. Отдельная оговорка: терминальный процесс придётся закрывать вручную — shell больше им не владеет (нынешний контракт владения описан в host.rs:18-21, 127-140). Поведение Tauri API `window.navigate` на произвольный URL из кода не устанавливалось из источников глубже вызова host.rs:114.

**Вывод:** сегодня attach-режима нет — shell всегда спавнит свой sidecar и парсит `dsh web: http://127.0.0.1:<port>` (host.rs:110-120, main.mjs:28-35); минимальная реализация сводится к env-условию вокруг одного вызова `window.navigate` в lib.rs/host.rs и тривиально выполнима.

---

## Q2. Гонки и конфликты

### (a) Два экземпляра приложения, каждый со своим sidecar — общее состояние на диске

Оба хоста наследуют один корень данных: приоритет `$DSH_HOME` → `~/.dsh` зашит в `dshHomePath`/`defaultDshHome` (packages/util/home-paths/src/index.ts:12-88); shell рабочую директорию берёт как `$DSH_HOME/workspace` или `~/.dsh/workspace` (host.rs:327-337), а сам `DSH_HOME` пробрасывает окружением. Реально наблюдаемый layout `~/.dsh`: `sessions/`, `storages/` (`workspace.json`, `session_projcache.json`, `usage-stats-cache.json`), `settings.yaml`, `skills/`, `.agent-presets/`.

Ключевой факт — явное отсутствие межпроцессной блокировки в JSON-доменном сторе: «No cross-process write locking: two processes writing the same root can interleave whole-file replacements (last write wins). Single-host-process deployments are the current consumer; the multi-process story is deferred» (packages/storage/storage-json/README.md:38). Каждый вызов записи атомичен (temp-write + fsync + `rename()`, storage-json/src/atomic.ts:24-34; README:9-11), но упорядочивание между вызовами — ответственность вызывающего.

Сценарии по слоям:

| Слой | Механика | Урон |
| --- | --- | --- |
| `storages/*.json` (json backend) | целофайловые replace, last-write-wins (storage-json/README.md:9,38) | потеря свежих записей реестра workspace (`workspace.json`) и projection-cache, но не порча формата файла |
| `~/.dsh/sessions/**` JSONL | append-only транскрипты, публикация нового файла без перезаписи (hard link / MoveFileExW, session-persistence-jsonl/README.md:42-43) | разные процессы пишут разные `id`-каталоги ⇒ аппенди-конфликта нет; риск появляется лишь при одинаковом session id, что практически исключено (uuid-каталоги) |
| project-каталоги вида `--<cwd>--/` | нормализация cwd lossy: «cwd strings that normalize alike share a project directory» (session-persistence-jsonl/README.md:19) | два приложения с тем же cwd пишут в общий проектный каталог — но id-подкаталоги различны, так что конфликт остаётся на уровне листингов |
| `settings.yaml`, `.agent-presets/`, skills | пишет человек/инструменты; двухписцевый сценарий — обычный last-save-wins | источники по locking этих файлов ничего не документируют — «не установлено из источников» |
| SQLite persistence (`storages`, если выбран бэкенд sqlite) | WAL + `busyTimeoutMs`, блокировки внутри БД (session-persistence-sqlite/README.md:37) | самый устойчивый слой, но даже там POSIX-проверки владельца/прав могут отказать второму процессу (README:37) |

Документированной гарантии single-instance нет нигде: `documented single-instance/locking guarantees` в README desktop-репозитория отсутствуют (README.md описывает функциональность, не конкурентность). Примечательно, что session-слой сам признаёт модель «один живой писец»: «a concurrently live session holding an open bracket over the same history has its own boundary elsewhere, so tolerating concurrent writers needs a liveness signal beyond the log» (docs/subsystems/session.md:124-125,593) — т.е. конкурентность писцов прямо вне охвата проектирования.

Severity: средняя и восстановимая — повреждения сводятся к проигрышу одной из сторон в whole-file replace; crash-repair чтения (sqlite: torn tail row, session-persistence-sqlite/README.md:19) существует, у json — fail-soft self-heal проекционного кэша (session-projection-cache/README.md:9,14).

### (b) Вкладки браузера + окно приложения на ОДНОМ хосте

Транспорт спроектирован под несколько клиентов:

- RPC stateless: каждый `/api` запрос независимо проходит gateway (`TypertGatewayService.invoke()` резолвит сервис per-call, packages/api/gateway/src/index.ts:97-121; README gateway: «resolves the current descriptor and Cordis Service for each call», src/index.ts:11).
- Даунлинк — WebSocket `/api/events.mux` + `/api/events.host`, у каждого клиента своя пара сокетов и своё «connection generation»; при обрыве поколение падает и перестраивается (packages/client/connection/README.md:13). Fan-out сделан на сервере: сокеты лишь доставляют ServerRequest-сообщения, клиент ничего не отправляет по ним.
- Read-model'ы живут на сервере: projection cache — персистентные чекпоинты «one record per session on the domain data form» (packages/session/session-projection-cache/README.md:5-14).

Что ломается при одновременной работе двух поверхностей в **одну** SessionId:

- конкурентные `send` в одну сессию: обе поверхности заводят активный turn/драйвер; семантика отмены определена вокруг «the active driver» — `cancel()` гасит очередь и активную работу (docs/subsystems/core.md:76-101), т.е. второй клиент способен отменить/перебить активность первого;
- plan/goal-каналы, стриминг чанков: события идут в mux каждого клиента, но перемешивание двух водителей turn в одном журнале никакой контракт не описывает — «не установлено из источников»;
- безопасное сосуществование — **разные** сессии: серверная авторитетность state + server-side projections делают независимые сессии изолированными; конкурентные cold reads одного списка лишь дублируют read-back («last write-back wins (rows are equivalent)», session-projection-cache/README.md:63).

### (c) То же самое: app-sidecar против терминального `dsh web`

Механика идентична — это тот же web-профиль и тот же бандл GUI (desktop работает строго на npm-версии `@deepseek-ai/dsh@0.1.1-rc.2`, host/package.json:8; терминал — на локально установленной версии, которая может отличаться — расхождения поведения версий «не установлено из источников»). Дополнительная разница одна: `DSH_HOME`. Sidecar наследует окружение от GUI-launch (launchd stub PATH восстанавливается только для PATH, host.rs:41-48, 65-67); если у пользователя в интерактивном shell экспортируется иной `DSH_HOME`, терминальный `dsh web` возьмёт его из профиля (resolution: explicit path → `$DSH_HOME` → `~/.dsh`, home-paths/src/index.ts:79-88), тогда как app-sidecar может увидеть другой/несуществующий `DSH_HOME` и попасть в другой корень — формально это не гонка, а тихое расщепление данных на два дома.

**Вывод:** (a) — два независимых хоста реально конкурируют за `~/.dsh/storages` без каких-либо блокировок (last-write-wins, storage-json/README.md:38), урон ограничен потерей записей, но сценарий явно вне проектного охвата; (b)/(c) — браузерный клиент мульти-клиентский по транспорту, безопасна работа с разными сессиями, одновременное управление одной сессией контрактно не покрыто.

---

## Q3. Горячие клавиши в приложении

### Что зарегистрировано сегодня

- Tauri-обвязка: поиск по `src-tauri/src` и `tauri.conf.json` на `shortcut|accelerator|menu` пуст; Cargo-зависимости содержат только `tauri`, `tauri-plugin-log`, `libc` (src-tauri/Cargo.toml:15-25) — плагины `global-shortcut`/`menu` не подключены.
- Capabilities: `default.json` выдаёт окну только `core:default` с комментарием, что страница говорит с хостом по HTTP/WS, а не Tauri IPC (src-tauri/capabilities/default.json) — никаких shortcut-permission грантов нет.
- Внутри WebView работают DOM-обработчики клиентских пакетов: Enter/Shift+Enter/Backspace/Delete/ArrowUp/Down/Escape/Cmd+Z арбитраж в поле ввода (packages/client/ui-conversation/src/client/skeleton/InputBar.tsx:348-398), клавиатурная навигация попапа команд (ui-commands PopupSelectView.tsx:82-114), Escape/click-outside в Modal/Menu/ModelSelect/HoverCard и др. (см. grep по `keydown` в packages/client/*/src). Глобальных (document/window-level) keydown-слушателей в client-пакетах не найдено — все хендлеры локальны внутри компонентов. Системный хоткей или command palette на глобальный акселератор — не установлено из источников (в репозитории не обнаружено).

### Варианты добавления (по официальной документации Tauri v2)

1. **`@tauri-apps/plugin-global-shortcut`** — системно-глобальные регистрации типа `CommandOrControl+Shift+X`; требует Rust-сторону (`tauri-plugin-global-shortcut` в Cargo.toml, `.plugin(tauri_plugin_global_shortcut::Builder...)`) плюс JS-guest `@tauri-apps/plugin-global-shortcut` и разрешение в capabilities. Источник: https://v2.tauri.app/plugin/global-shortcut/ . Trade-off: работает в масштабе ОС (даже без фокуса), но это shell-изменения + переустановка capabilities; для «внутри приложения» избыточен.
2. **Window-local accelerators / меню** — accelerate'ры пунктов меню окна/трея и обработчики через `on_menu_event`/`on_window_event` определяются в Rust-коде shell; документация: https://v2.tauri.app/learn/window-menu/ и https://v2.tauri.app/reference/rust/tauri/menu/ . Trade-off: тоже правки Rust + пересборка; зато не нужно системно-глобальной регистрации и работает стандартными средствами ОС.
3. **Plain DOM `keydown` в client-плагинах** — динамический Cordis client-плагин, слушающий `keydown`/`keyup` в своём fiber'е (паттерн ctx.effect-управляемых listeners), меняет только harness-composition, никакого rebuild шелла. Поскольку весь UI — это web bundle под `window.__DSH_BOOT__` (README.md: «WebView 直接访问 ... 不改动 deepseek-harness 一行代码»), DOM-подход покрывает любые действия внутри окна: хоткеи вида Cmd+K, palette toggle, navigation. Ограничение — только когда окно в фокусе и WebView получил событие.

Для типичного запроса «добавить Cmd+K / jump-to-session» вариант 3 — минимум усилий и обратимо; вариант 1 нужен лишь для вызова при свернутом приложении.

**Вывод:** в шелле сегодня ноль shortcut-инфраструктуры (нет ни плагина, ни capability, ни menu), вся действующая клавиатура — DOM-обработчики web-бандла (InputBar.tsx:348-398 и компоненты ui-primitives/ui-commands); добавить новые можно либо плагином global-shortcut и меню в Rust (https://v2.tauri.app/plugin/global-shortcut/, https://v2.tauri.app/learn/window-menu/), либо без единого изменения шелла — DOM keydown в client-плагине, что предпочтительно для действий внутри окна.

---

## Итоговая сводка

| Вопрос | Вердикт |
| --- | --- |
| Q1 | attach-режима нет; минимальная реализация — точечная правка `lib.rs`/`host.rs` вокруг `window.navigate`, feasibility высокая |
| Q2(a) | два своих sidecar гонят `~/.dsh/storages` last-write-wins без блокировок (storage-json/README.md:38); потери записей, не порча форматов |
| Q2(b)/(c) | транспорт мульти-клиентский; одна сессия из двух поверхностей — вне контракта; app vs terminal отличается только возможным расхождением `DSH_HOME`/версии |
| Q3 | шелл-хоткеев нет; для in-window использовать DOM keydown, global-shortcut — только для system-wide |
