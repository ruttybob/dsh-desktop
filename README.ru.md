<div align="center">

# dsh-desktop

**DeepSeek Harness Desktop** — нативное окно Tauri поверх [deepseek-harness](https://github.com/deepseek-ai/deepseek-harness)

[![Release](https://img.shields.io/github/v/release/kyorakuyk/dsh-desktop?label=release)](https://github.com/kyorakuyk/dsh-desktop/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/kyorakuyk/dsh-desktop/ci.yml?label=ci)](https://github.com/kyorakuyk/dsh-desktop/actions/workflows/ci.yml)
[![License](https://img.shields.io/github/license/kyorakuyk/dsh-desktop)](LICENSE)
[![Topic](https://img.shields.io/badge/topic-dsh--plugin-blue)](#)

Windows · macOS · Linux — работает из коробки, установка Node.js не требуется

[English version](README.md)

</div>

---

## Что это

dsh-desktop помещает Web GUI DeepSeek Harness (`dsh web`) в нативное окно рабочего стола:

- **Оболочка Tauri 2 (Rust)** отвечает за окно, жизненный цикл хост-процесса и упаковку;
- **Встроенный Node.js-хост (sidecar)** запускает опубликованный на npm пакет [`@deepseek-ai/dsh`](https://www.npmjs.com/package/@deepseek-ai/dsh) (`dsh web --port 0`);
- **WebView напрямую открывает `http://127.0.0.1:<случайный порт>`** и в полной мере использует
  инъекцию `window.__DSH_BOOT__`, раздачу plugin bundle, `/api` JSON-RPC и WebSocket-поток событий harness — **без изменения ни одной строки deepseek-harness**.

Установщик содержит Node-рантайм и все зависимости harness, поэтому на машине пользователя ничего предварительно ставить не нужно.

## ✨ Возможности

| Возможность | Описание |
| --- | --- |
| 🖥️ Нативное окно | Windows WebView2 / macOS WKWebView / Linux WebKitGTK |
| 📦 Установка без зависимостей | Установщик включает Node-рантайм + полный набор `@deepseek-ai/dsh` |
| 🔌 Полные возможности harness | Сессии, вызовы инструментов, плагины, настройка моделей, рабочая область — всё как в `dsh web` |
| 🚀 Готово сразу после запуска | Splash-экран → сборка хоста (~30 строк плагинов) → автоматический вход в GUI |
| 🤖 Настройка моделей | API Key настраивается прямо в GUI: Настройки → Модель |
| 🔄 Кроссплатформенные релизы | GitHub Actions по тегу собирает установщики для трёх платформ и публикует в Releases |
| 📋 Логи | Логи хоста и оболочки выводятся единообразно (tauri-plugin-log) |

## 📥 Загрузка и установка

Скачайте установщик для своей платформы со страницы [Releases](https://github.com/kyorakuyk/dsh-desktop/releases/latest):

| Платформа | Файл | Описание |
| --- | --- | --- |
| Windows | `dsh-desktop_<version>_x64-setup.exe` | Установщик NSIS, установка двойным кликом |
| macOS (Apple Silicon) | `dsh-desktop_<version>_aarch64.dmg` | Перетащите в Applications |
| macOS (Intel) | `dsh-desktop_<version>_x64.dmg` | Аналогично |
| Linux | `dsh-desktop_<version>_amd64.deb` | Debian / Ubuntu: `sudo dpkg -i` |
| Linux | `dsh-desktop-<version>-1.x86_64.rpm` | Fedora / RHEL: `sudo rpm -i` |

> **Первый запуск**: после старта подождите несколько секунд (сборка хоста), затем задайте API Key в Настройки → Модель — и можно общаться.
> По умолчанию данные хранятся в `~/.dsh` (сессии, настройки, profile; общие с dsh CLI).

## 🏗️ Архитектура

```
┌─────────────────────────────────────────────┐
│ Окно Tauri (WebView2 / WKWebView)           │
│  └─ загружает http://127.0.0.1:<порт>       │
├─────────────────────────────────────────────┤
│ Оболочка Rust (src-tauri)                   │
│  ├─ запускает/следит за sidecar, парсит     │
│  │   строку `dsh web:` с URL                │
│  ├─ рабочая область ~/.dsh/workspace        │
│  ├─ завершает хост-процесс при выходе       │
│  └─ логирование (tauri-plugin-log)          │
├─────────────────────────────────────────────┤
│ Sidecar: встроенный Node + @deepseek-ai/dsh │
│  └─ node host/main.mjs → dsh web --port 0   │
│     ├─ инъекция __DSH_BOOT__ (dsh-client-   │
│     │   modules сканирует dsh.client)       │
│     ├─ /plugins/<id>/client.js plugin bundle│
│     ├─ /api шлюз JSON-RPC                   │
│     └─ /api/events.mux|host WebSocket-поток │
└─────────────────────────────────────────────┘
```

### Последовательность запуска

1. Приложение стартует, окно показывает **splash-экран** (загрузочный интерфейс на время сборки хоста);
2. Оболочка Rust запускает встроенный `node.exe` → `host/main.mjs` → в этом процессе выполняется `dsh web --port 0`;
3. `dsh web` собирает дерево плагинов Cordis (два bundle — `@deepseek-ai/dsh-base` + `@deepseek-ai/dsh-web-app`,
   около 30 строк плагинов), webserver привязывается к **случайному порту, выделенному ОС**;
4. После стабилизации дерева Loader bundle web-app печатает `dsh web: http://127.0.0.1:<port>`;
5. Оболочка Rust парсит эту строку → WebView переходит по URL → GUI harness загружается полностью
   (boot manifest, предзагрузка plugin bundle, соединение `/api`, WebSocket-поток событий);
6. Пользователь закрывает окно → приложение завершается → хост-процесс останавливается (данные сессий уже сохранены на диск).

### Технические решения

- **Ноль конфликтов портов**: `--port 0` позволяет ОС выделить порт, Rust парсит реальный адрес из stdout;
- **Префикс `\\?\` в Windows**: пути ресурсов Tauri содержат префикс расширенной длины, который Node loader не может разрешить;
  он срезается перед передачей дочернему процессу (`strip_verbatim_prefix`);
- **Компактный bundle**: pnpm устанавливает зависимости в `hoisted`-разметке (без симлинков после копирования); при упаковке исключаются
  store `.pnpm` (~250 МБ) и npm/npx/corepack из дистрибутива Node (~30 МБ);
- **Smoke-тест без ключа**: в CI и локально `npm run smoke` проверяет всю цепочку хоста
  (запуск → строка URL → index 200 + shell HTML), API Key не требуется.

## 📁 Структура репозитория

```
dsh-desktop/
├── src-tauri/            # Оболочка Rust (окно, жизненный цикл sidecar, конфигурация упаковки, иконки)
│   ├── src/host.rs       # запуск sidecar / парсинг URL / завершение процесса
│   ├── src/lib.rs        # сборка приложения Tauri и хуки выхода
│   └── resources/        # артефакты, собираемые при сборке (gitignored):
│                         #   host/{main.mjs, node/, node_modules/}
├── host/                 # Точка входа Node-хоста
│   ├── main.mjs          # выполняет dsh web внутри процесса (перенаправление argv + импорт file://)
│   └── pnpm-workspace.yaml  # настройки pnpm 11 (hoisted / allowBuilds / порог возраста публикации)
├── scripts/
│   ├── fetch-node.mjs    # скачивает официальный Node-рантайм (v22 LTS, по платформе/архитектуре)
│   ├── bundle-host.mjs   # собирает resources/host (исключает .pnpm и файлы npm из дистрибутива)
│   └── smoke-host.mjs    # smoke-тест хоста без ключа (приоритет — упакованным артефактам)
├── ui/                   # Splash-экран (чистая статика, без шага сборки)
├── .github/workflows/
│   ├── release.yml       # тег → tauri-action сборка на трёх платформах → GitHub Releases
│   └── ci.yml            # PR/push: host smoke + cargo check на Windows/Linux
└── package.json          # вспомогательные скрипты (см. ниже)
```

## 🛠️ Разработка

### Требования

| Зависимость | Версия | Описание |
| --- | --- | --- |
| Node.js | ≥ 22.19 | вместе с npm |
| pnpm | ≥ 11 | для установки зависимостей host |
| Rust toolchain | ≥ 1.77 | cargo / rustc |
| Доп. зависимости Linux | — | `libwebkit2gtk-4.1-dev` и др., см. [официальную документацию Tauri](https://tauri.app/start/prerequisites/) |

### Быстрый старт

```sh
# 1. Установите зависимости и соберите ресурсы хоста (Node-рантайм + @deepseek-ai/dsh и его зависимости)
npm install
npm run host:install        # внутри: pnpm -C host install --prod
npm run host:bundle         # результат: src-tauri/resources/host/ (в gitignore)

# 2. Запуск в режиме разработки (открывает окно; хост запускается Rust автоматически)
npm run tauri dev
```

### Основные скрипты

| Скрипт | Назначение |
| --- | --- |
| `npm run host:install` | Установить зависимости host (версия `@deepseek-ai/dsh` зафиксирована) |
| `npm run host:fetch-node` | Скачать/обрезать официальный Node-рантайм |
| `npm run host:bundle` | Собрать `src-tauri/resources/host/` |
| `npm run smoke` | Smoke-тест хоста без ключа (приоритет — упакованным артефактам) |
| `npm run tauri dev` | Запуск в режиме разработки |
| `npm run build` | Продакшен-сборка (bundle host + tauri build) |

### Сборка установщиков

```sh
npm run build
# Windows: src-tauri/target/release/bundle/nsis/*.exe
# macOS:   bundle/macos/*.app + bundle/dmg/*.dmg
# Linux:   bundle/deb/*.deb + bundle/rpm/*.rpm (AppImage отложено, см. известные ограничения)
```

## 🚀 Публикация на GitHub

В репозитории настроен `.github/workflows/release.yml`. Два способа запуска:

```sh
# Способ 1: запушить тег (рекомендуется)
git tag v0.2.0
git push origin v0.2.0

# Способ 2: запустить release workflow вручную на странице Actions
```

Workflow собирает установщики на Windows / macOS (arm64+x64) / Linux и загружает их в GitHub
Releases (черновик; после проверки опубликуйте вручную).

> Опциональные secrets (нужны только для автообновления, см. ниже):
> `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.

## 🧪 Проверки CI (ci.yml)

| Job | Содержание |
| --- | --- |
| Host boot smoke | На Ubuntu устанавливает зависимости host и запускает `dsh web`, проверяет строку URL + shell HTML |
| cargo check | Проверка компиляции на Windows + Ubuntu (включая проверку ресурсов в build.rs) |

## 🔄 Автообновление (дорожная карта)

В текущей версии `tauri-plugin-updater` не скомпилирован. Шаги для включения:

1. `cargo add tauri-plugin-updater` и регистрация в `src-tauri/src/lib.rs`;
2. Сгенерировать пару ключей `npx tauri signer generate -w ~/.tauri/dsh-desktop.key`,
   публичный ключ записать в `tauri.conf.json → plugins.updater.pubkey`;
3. Настроить `TAURI_SIGNING_PRIVATE_KEY` и `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` в Secrets репозитория;
4. В `tauri.conf.json` указать `plugins.updater.endpoints` на
   `https://github.com/kyorakuyk/dsh-desktop/releases/latest/download/latest.json`;
5. Запушить новый тег — `tauri-action` автоматически загрузит манифест обновлений и подписанные артефакты.

## 🔍 Устранение неполадок

| Симптом | Решение |
| --- | --- |
| Долго висит splash-экран | Проверьте логи приложения, чтобы найти причину сбоя запуска хоста |
| Ошибка запуска хоста | Логи: Windows `%LOCALAPPDATA%\com.dsh.desktop\logs\dsh-desktop.log`; macOS/Linux см. `~/Library/Logs` и `~/.local/share/com.dsh.desktop/logs` |
| В Attach Mode: «dsh web authentication required» | Сервер требует токен. Первое подключение — только по URL **целиком** из строки `dsh web: http://…?token=…` (токен меняется при каждом перезапуске сервера, но нужен один раз: далее работает подписанная cookie). Меню **Change Server…** (⌘/Ctrl+Shift+C) возвращает к форме подключения с любой страницы; форма не запоминает URL, на котором не прошло рукопожатие |
| Модель не отвечает | Проверьте API Key и настройки модели в Настройки → Модель |
| CLI, видимый в терминале (например, `bd`), не виден в приложении | GUI при запуске наследует урезанный `PATH` launchd из четырёх каталогов; хост при старте определяет PATH через login shell и восстанавливает пользовательский `PATH`. Если отдельный CLI всё же отсутствует, убедитесь, что он есть в `PATH` login shell, а его profile-скрипт не блокируется дольше 5 секунд |
| При сборке: host bundle missing | Сначала выполните `npm run host:bundle` (`build.rs` сообщит об этом явно) |

## ⚠️ Известные ограничения

- Первый запуск занимает несколько секунд (хост собирает ~30 строк плагинов + предзагрузка frontend bundle); всё это время показывается splash-экран;
- Большой размер установщика (внутри Node-рантайм и все зависимости harness, порядка ~100 МБ);
- При аварийном завершении хоста окно остаётся на последней странице (причина видна в логах);
- Linux AppImage пока не предоставляется: в CI падает упаковка `linuxdeploy` (deb/rpm работают нормально,
  проблема в инструментарии AppImage), будет исправлено в следующих версиях;
- Автообновление не включено (см. дорожную карту выше).

## 📄 Лицензия

[MIT](LICENSE) — как у deepseek-harness.

> **Отказ от ответственности**: этот проект — неофициальная настольная обёртка от сообщества, не связанная с DeepSeek;
> DeepSeek Harness и связанные товарные знаки принадлежат их владельцам.
