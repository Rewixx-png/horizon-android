//! The Android launcher's user-facing language. The game keeps its own
//! language setting; this module only covers the native launcher, which used
//! to hard-code English strings in its drawing and status code.
//!
//! Call sites use [`tr`] with an English key and get the Russian string from
//! the table below, or the English key when the table has no entry. Templates
//! use `{name}` placeholders; the [`tr!`] macro substitutes them, so both
//! languages must carry the same placeholders (a test enforces that).

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    English,
    Russian,
}

impl Language {
    pub const ALL: [Self; 2] = [Self::English, Self::Russian];

    /// The name shown in the language picker, in that language itself.
    pub const fn label(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Russian => "Русский",
        }
    }

    pub const fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Russian => "ru",
        }
    }
}

/// English key to Russian translation. Order is irrelevant; lookups are a
/// linear scan over a small table. Keep keys identical to the literals passed
/// to [`tr`] so a renamed string cannot silently fall back to English.
const RU: &[(&str, &str)] = &[
    // Shell and pages.
    ("Play", "Играть"),
    ("Mods", "Моды"),
    ("Packs", "Паки"),
    ("Shaders", "Шейдеры"),
    ("Installed", "Установлено"),
    ("Settings", "Настройки"),
    ("Files", "Файлы"),
    ("Setup", "Настройка"),
    // Play page and install progress.
    ("Getting your game ready", "Готовим игру"),
    ("Ready to play", "Можно играть"),
    ("Your Java worlds", "Ваши Java-миры"),
    ("Your Java worlds, on Android", "Ваши Java-миры на Android"),
    (
        "Minecraft 26.3  /  Profile {profile}",
        "Minecraft 26.3  /  Профиль {profile}",
    ),
    ("Step {current} of {total}", "Шаг {current} из {total}"),
    ("Retry installation", "Повторить установку"),
    ("Download Minecraft 26.3", "Скачать Minecraft 26.3"),
    ("Download resources from Mojang", "Скачать ресурсы с Mojang"),
    (
        "Install and verify on this phone",
        "Установить и проверить на телефоне",
    ),
    (
        "Play offline or sign in for servers",
        "Играть офлайн или войти для серверов",
    ),
    (
        "Open a world or join a Java server",
        "Открыть мир или зайти на Java-сервер",
    ),
    (
        "Touch setup: Options > Controls",
        "Настройка касаний: «Параметры» > «Управление»",
    ),
    (
        "Find packs and .hmod in the tabs",
        "Паки и .hmod — во вкладках",
    ),
    (
        "Please keep Horizon open. Downloaded files are kept for retry.",
        "Не закрывайте Horizon. Скачанные файлы сохранятся для повтора.",
    ),
    (
        "{done} / {total} MiB  - verified downloads",
        "{done} / {total} МиБ — проверено",
    ),
    (
        "{done} / {total} control files verified",
        "{done} / {total} файлов управления проверено",
    ),
    ("Checking Minecraft version", "Проверка версии Minecraft"),
    (
        "Downloading game resources and sound",
        "Загрузка ресурсов и звуков игры",
    ),
    ("Unpacking verified files", "Распаковка проверенных файлов"),
    (
        "Preparing worlds and registries",
        "Подготовка миров и реестров",
    ),
    (
        "Downloading touch controls",
        "Загрузка сенсорного управления",
    ),
    ("Finishing installation", "Завершение установки"),
    // Catalog pages.
    ("Horizon Mods", "Моды Horizon"),
    ("Shader Packs", "Наборы шейдеров"),
    ("Resource Packs", "Наборы ресурсов"),
    ("Search / .hmod URL: ", "Поиск / URL .hmod: "),
    ("Search: ", "Поиск: "),
    ("Search: {}", "Поиск: {}"),
    ("Import .hmod", "Импорт .hmod"),
    ("Search", "Найти"),
    ("Open folder", "Открыть папку"),
    ("Import file", "Импорт файла"),
    (
        "Verified downloads / enable in Installed",
        "Проверенные загрузки / включите в «Установлено»",
    ),
    (
        "Modrinth Java mods are browse-only; Fabric/Forge/NeoForge cannot run here",
        "Java-моды Modrinth — только просмотр",
    ),
    (
        "Search Horizon mods or paste an HTTPS .hmod link",
        "Моды Horizon или HTTPS-ссылка .hmod",
    ),
    (
        "Modrinth shader ZIPs / experimental compatibility; some packs may not work",
        "ZIP-шейдеры Modrinth: экспериментально",
    ),
    ("Horizon native .hshader packs", "Нативные паки .hshader"),
    ("Java mod", "Java-мод"),
    (".hnmod: later", ".hnmod: позже"),
    ("64-bit required", "Нужен 64-битный APK"),
    ("Install", "Установить"),
    ("Retry", "Повторить"),
    ("Previous", "Назад"),
    ("Next", "Далее"),
    (
        "No Horizon shader packs match this search. Try another query or import a .hshader.",
        "Шейдеры Horizon не найдены. Измените запрос или импортируйте .hshader.",
    ),
    (
        "No results. Try another query or import a file.",
        "Ничего не найдено. Измените запрос или импортируйте файл.",
    ),
    // Installed page.
    ("Import", "Импорт"),
    ("Folder", "Папка"),
    ("Remove", "Удалить"),
    // Settings page.
    ("Launch settings", "Настройки запуска"),
    ("Rendering", "Рендеринг"),
    ("Client resolution", "Разрешение клиента"),
    ("Profile", "Профиль"),
    ("Orientation", "Ориентация"),
    ("Input", "Ввод"),
    ("Immersive fullscreen", "Полноэкранный режим"),
    ("Avoid camera cutout", "Обходить вырез камеры"),
    ("Aspect ratio", "Соотношение сторон"),
    ("Display refresh rate", "Частота экрана"),
    ("Performance hints", "Подсказки производительности"),
    ("GPU profiling", "Профилирование GPU"),
    ("Performance recording", "Запись производительности"),
    ("Diagnostic report", "Диагностический отчёт"),
    ("Performance report", "Отчёт производительности"),
    ("Server", "Сервер"),
    ("Language", "Язык"),
    ("Microsoft / Xbox account", "Аккаунт Microsoft"),
    ("Diagnostics", "Диагностика"),
    ("On", "Вкл"),
    ("Off", "Выкл"),
    ("On (ADPF)", "Вкл (ADPF)"),
    ("On / included in report", "Вкл / в отчёте"),
    ("On / up to 30 min", "Вкл / до 30 мин"),
    (
        "Singleplayer / tap to enter server",
        "Одиночная игра / нажмите, чтобы ввести сервер",
    ),
    (
        "Export ZIP / review before sharing",
        "Экспорт ZIP / проверьте перед отправкой",
    ),
    ("Open in Files / Reports", "Открыть в «Файлах» / «Отчётах»"),
    ("System default", "Системная"),
    ("Profile {id}", "Профиль {id}"),
    ("Default profile", "Профиль по умолчанию"),
    (
        "Separate saves, mods and packs",
        "Отдельные сохранения, моды и паки",
    ),
    ("Create profile", "Создать профиль"),
    ("Create Profile {id}", "Создать профиль {id}"),
    ("Choose profile", "Выбор профиля"),
    (
        "Each profile keeps its own game settings and saves.",
        "У каждого профиля свои настройки и сохранения.",
    ),
    (
        "Applies to the launcher. The game has its own language setting.",
        "Применяется к лаунчеру. В игре язык выбирается отдельно.",
    ),
    // Explicit choosers.
    (
        "Fits the entire picture without stretching. Touch coordinates follow the picture. Portrait swaps width and height.",
        "Вписывает картинку без растяжения. Касания следуют за картинкой. В портретном режиме ширина и высота меняются местами.",
    ),
    (
        "An Android display preference, not a game FPS cap. Unsupported rates may be ignored.",
        "Настройка экрана Android, а не ограничение FPS. Неподдерживаемые частоты могут игнорироваться.",
    ),
    ("{hz} Hz", "{hz} Гц"),
    ("Native resolution", "Нативное разрешение"),
    (
        "Lower resolution for the whole picture",
        "Пониженное разрешение всей картинки",
    ),
    ("Custom percentage...", "Свой процент…"),
    (
        "Enter a value from 25 to 100",
        "Введите значение от 25 до 100",
    ),
    (
        "Small screens retain a usable menu size.",
        "На маленьких экранах меню остаётся читаемым.",
    ),
    ("Input mode", "Режим ввода"),
    (
        "Mouse mode uses a physical keyboard. Touch mode keeps the Android keyboard.",
        "Режим мыши использует физическую клавиатуру. Сенсорный режим оставляет клавиатуру Android.",
    ),
    ("Screen orientation", "Ориентация экрана"),
    (
        "Applies to the launcher and game. Does not change touch bindings.",
        "Применяется к лаунчеру и игре. Не меняет сенсорные привязки.",
    ),
    ("Rendering mode", "Режим рендеринга"),
    (
        "Uses the system GPU driver. Applies on the next game launch.",
        "Использует системный драйвер GPU. Применится при следующем запуске игры.",
    ),
    ("OpenGL ES / experimental", "OpenGL ES / эксперимент"),
    (
        "For older GPUs. Requires ES 3.0+ and sufficient driver limits.",
        "Для старых GPU. Нужен ES 3.0+ и достаточные лимиты драйвера.",
    ),
    ("Vulkan / GPU batching", "Vulkan / пакетная отрисовка"),
    (
        "Groups terrain draws and enables supported GPU culling.",
        "Объединяет отрисовку земли и включает поддерживаемое GPU-отсечение.",
    ),
    ("Vulkan / compatibility", "Vulkan / совместимость"),
    (
        "Direct terrain draws for drivers with missing or transparent blocks.",
        "Прямая отрисовка земли для драйверов с пропадающими или прозрачными блоками.",
    ),
    ("Remove add-on?", "Удалить дополнение?"),
    (
        "Deletes only the selected file from this profile. This cannot be undone.",
        "Удаляет только выбранный файл из этого профиля. Отменить нельзя.",
    ),
    ("Cancel", "Отмена"),
    ("Automatic", "Автоматически"),
    ("Touch & Android keyboard", "Касания и клавиатура Android"),
    ("Keyboard & mouse", "Клавиатура и мышь"),
    ("Controller", "Контроллер"),
    ("Landscape (sensor)", "Альбомная (датчик)"),
    ("Portrait (sensor)", "Портретная (датчик)"),
    ("Landscape (reversed)", "Альбомная (обратная)"),
    ("Portrait (reversed)", "Портретная (обратная)"),
    ("Automatic rotation", "Автоповорот"),
    // Accounts overlay.
    ("Accounts", "Аккаунты"),
    ("Offline player name", "Имя для офлайн-игры"),
    (
        "Used when signed out of Microsoft.",
        "Используется без входа в Microsoft.",
    ),
    ("Done", "Готово"),
    ("Signed in: {name}", "Вход выполнен: {name}"),
    ("Sign in required: {name}", "Требуется вход: {name}"),
    ("Signing in...", "Вход…"),
    ("Offline: {name}", "Офлайн: {name}"),
    ("Offline / Sign in", "Офлайн / Войти"),
    (
        "Account saved securely. Sessions renew automatically.",
        "Аккаунт сохранён безопасно. Сессия продлевается автоматически.",
    ),
    (
        "Your password is entered in Microsoft's browser.",
        "Пароль вводится в браузере Microsoft.",
    ),
    ("Requesting a sign-in code...", "Запрос кода входа…"),
    ("{code}    /    Copy code", "{code}    /    Копировать код"),
    (
        "Code copied. Paste it on the Microsoft page.",
        "Код скопирован. Вставьте его на странице Microsoft.",
    ),
    (
        "Could not copy the code. Try again.",
        "Не удалось скопировать код. Попробуйте ещё раз.",
    ),
    (
        "Request Minecraft API approval",
        "Запросить доступ к Minecraft API",
    ),
    (
        "Copy code and open browser",
        "Копировать код и открыть браузер",
    ),
    (
        "Restart browser sign-in",
        "Перезапустить вход через браузер",
    ),
    ("Switch Microsoft account", "Сменить аккаунт Microsoft"),
    ("Sign in with browser", "Войти через браузер"),
    ("Use a device code instead", "Войти по коду устройства"),
    ("Cancel sign-in", "Отменить вход"),
    ("Sign out", "Выйти"),
    ("Close", "Закрыть"),
    ("Back (sign-in continues)", "Назад (вход продолжится)"),
    ("Signed in as {name}", "Вход выполнен: {name}"),
    (
        "Signed out. Offline play is available.",
        "Вы вышли. Доступна офлайн-игра.",
    ),
    (
        "Register our Microsoft public client application first, then enter its client ID.",
        "Сначала зарегистрируйте публичное приложение Microsoft, затем введите его client ID.",
    ),
    (
        "Session ended. Sign in again, or sign out to play offline.",
        "Сессия истекла. Войдите снова или выйдите для офлайн-игры.",
    ),
    ("Enter an offline name", "Введите офлайн-имя"),
    // Status lines.
    (
        "Preparing Microsoft session...",
        "Подготовка сессии Microsoft…",
    ),
    (
        "This APK is missing its engine data. Install a complete build.",
        "В APK нет данных движка. Установите полную сборку.",
    ),
    (
        "Saved {name} in Reports. Review the ZIP before sharing; nothing was uploaded.",
        "Файл {name} сохранён в «Отчётах». Проверьте ZIP перед отправкой; ничего не отправлялось.",
    ),
    (
        "Diagnostic export failed: {error}",
        "Не удалось экспортировать диагностику: {error}",
    ),
    (
        "File export failed: {error}",
        "Не удалось экспортировать файл: {error}",
    ),
    (
        "Repository opened in browser.",
        "Репозиторий открыт в браузере.",
    ),
    (
        "No browser available to open the repository.",
        "Нет браузера, чтобы открыть репозиторий.",
    ),
    (
        "Opened in Files. Return here after copying files.",
        "Открыто в «Файлах». Вернитесь после копирования файлов.",
    ),
    (
        "Open the system Files app and choose Horizon Android.",
        "Откройте системные «Файлы» и выберите Horizon Android.",
    ),
    (
        "ADPF gives Android CPU frame-work hints on supported devices. It does not cap FPS or guarantee a speedup.",
        "ADPF передаёт Android подсказки о загрузке CPU на поддерживаемых устройствах. Это не ограничивает FPS и не гарантирует ускорение.",
    ),
    (
        "Detailed GPU profiling records diagnostics and has overhead. Reports remain on this device.",
        "Подробное профилирование GPU пишет диагностику и создаёт нагрузку. Отчёты остаются на устройстве.",
    ),
    (
        "Performance recording off.",
        "Запись производительности выключена.",
    ),
    (
        "Recording starts on Play. Detailed timings add overhead. Return here to save the report.",
        "Запись начнётся при запуске игры. Подробные тайминги создают нагрузку. Вернитесь сюда, чтобы сохранить отчёт.",
    ),
    (
        "Reports opened in Files. Copy or share a report from there.",
        "Отчёты открыты в «Файлах». Скопируйте или отправьте отчёт оттуда.",
    ),
    (
        "Last handled error: {error}. {summary}",
        "Последняя ошибка: {error}. {summary}",
    ),
    (
        "Preparing local diagnostic ZIP. No upload; review before sharing.",
        "Готовим локальный диагностический ZIP. Без отправки; проверьте перед передачей.",
    ),
    (
        "Sandboxed code mods require a 64-bit APK",
        "Изолированные моды требуют 64-битного APK",
    ),
    (
        "Press Import .hmod to download this HTTPS link",
        "Нажмите «Импорт .hmod», чтобы скачать эту HTTPS-ссылку",
    ),
    (
        "Searching... You can switch tabs while results load.",
        "Поиск… Можно переключать вкладки, пока загружаются результаты.",
    ),
    ("Horizon mod exceeds 32 MiB", "Мод Horizon превышает 32 МиБ"),
    (
        "Installed {name}. Enable it in Installed after reviewing its capabilities in game Mods.",
        "Установлено: {name}. Включите в «Установлено», проверив возможности в игровых модах.",
    ),
    (
        "This .hnmod needs a separately compiled client. Android support is planned; sandboxed .hmod mods can be installed now.",
        "Для .hnmod нужен отдельно собранный клиент. Поддержка Android запланирована; .hmod-моды можно ставить уже сейчас.",
    ),
    (
        "Installed {count} item(s). Open Play to load this profile.",
        "Установлено: {count}. Откройте «Играть», чтобы загрузить профиль.",
    ),
    (
        "Selection saved for the next game launch",
        "Выбор сохранён до следующего запуска игры",
    ),
    ("Invalid content filename", "Недопустимое имя файла"),
    (
        "Refusing a redirected content directory",
        "Каталог контента перенаправлен — отказ",
    ),
    (
        "Only an explicitly selected regular add-on file can be removed",
        "Можно удалить только явно выбранный обычный файл дополнения",
    ),
    ("Removed {name}", "Удалено: {name}"),
    (
        "Unsupported download source",
        "Источник загрузки не поддерживается",
    ),
    (
        "Checking compatibility and downloading...",
        "Проверка совместимости и загрузка…",
    ),
    (
        "Downloading sandboxed .hmod over HTTPS...",
        "Загрузка .hmod по HTTPS…",
    ),
    (
        "Downloading experimental shader ZIP...",
        "Загрузка экспериментального ZIP-шейдера…",
    ),
    (
        "{count} Java mods - browsing only; this Rust client cannot run Java loaders",
        "{count} Java-модов — только просмотр; Rust-клиент не запускает Java-лоадеры",
    ),
    (
        "{count} results - select Install to check compatibility",
        "{count} результатов — нажмите «Установить» для проверки совместимости",
    ),
    (
        "{hmod} .hmod / {hnmod} source-only .hnmod (not supported yet)",
        "{hmod} .hmod / {hnmod} .hnmod только с исходником (пока не поддерживается)",
    ),
    (
        "Mods, resource packs and shaders for this profile",
        "Моды, ресурсы и шейдеры этого профиля",
    ),
    ("No items installed yet", "Пока ничего не установлено"),
    ("Display", "Экран"),
    ("Performance", "Производительность"),
    ("Support", "Поддержка"),
    ("Network", "Сеть"),
    ("Account", "Аккаунт"),
    (
        "Display, input and performance",
        "Экран, ввод и производительность",
    ),
    ("{hz} Hz (request)", "{hz} Гц (запрос)"),
    ("Native", "Нативно"),
    (
        "New version {version} is available",
        "Доступна новая версия {version}",
    ),
    ("New version available", "Доступно обновление"),
    ("Download", "Скачать"),
    ("Download and install", "Скачать и установить"),
    ("Update downloaded", "Обновление скачано"),
    ("Ready to install", "Можно устанавливать"),
    (
        "Allow installs from Horizon in Android settings, then tap Retry installation.",
        "Разрешите установку из Horizon в настройках Android и нажмите «Повторить установку».",
    ),
    (
        "Tap to download and install",
        "Нажмите, чтобы скачать и установить",
    ),
    (
        "Downloading update... {percent}%",
        "Загрузка обновления… {percent}%",
    ),
    ("Update failed", "Ошибка обновления"),
    ("Update failed: {error}", "Ошибка обновления: {error}"),
    (
        "The installer could not be opened. Allow installs from Horizon and try again.",
        "Не удалось открыть установщик. Разрешите установку из Horizon и попробуйте снова.",
    ),
];

/// Translate one fixed English launcher string.
pub fn tr(language: Language, text: &'static str) -> &'static str {
    if language == Language::English {
        return text;
    }
    RU.iter()
        .find_map(|(english, russian)| (*english == text).then_some(*russian))
        .unwrap_or(text)
}

/// The raw translation table, for the completeness tests.
pub fn translations() -> &'static [(&'static str, &'static str)] {
    RU
}

/// Translate a fixed string, or a `{name}` template with named values.
///
/// ```
/// use horizon_client::{launcher_i18n::Language, tr};
/// assert_eq!(tr!(Language::Russian, "Play"), "Играть");
/// let text = tr!(
///     Language::Russian,
///     "Step {current} of {total}",
///     { current = 2, total = 6 }
/// );
/// assert_eq!(text, "Шаг 2 из 6");
/// ```
#[macro_export]
macro_rules! tr {
    ($language:expr, $text:literal) => {
        $crate::launcher_i18n::tr($language, $text)
    };
    ($language:expr, $text:literal, { $($name:ident = $value:expr),+ $(,)? }) => {{
        let mut translated = $crate::launcher_i18n::tr($language, $text).to_owned();
        $(
            {
                let value = $value.to_string();
                translated = translated.replace(
                    concat!("{", stringify!($name), "}"),
                    value.as_str(),
                );
            }
        )+
        translated
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn placeholders(text: &str) -> HashSet<&str> {
        let mut found = HashSet::new();
        let mut rest = text;
        while let Some(start) = rest.find('{') {
            let tail = &rest[start + 1..];
            let Some(end) = tail.find('}') else {
                break;
            };
            found.insert(&tail[..end]);
            rest = &tail[end + 1..];
        }
        found
    }

    #[test]
    fn english_is_the_identity_and_unknown_keys_fall_back() {
        assert_eq!(tr(Language::English, "Play"), "Play");
        assert_eq!(tr(Language::Russian, "Play"), "Играть");
        assert_eq!(
            tr(Language::Russian, "Not a launcher key"),
            "Not a launcher key"
        );
    }

    #[test]
    fn every_russian_entry_is_non_empty_and_unique() {
        let mut keys = HashSet::new();
        for (english, russian) in translations() {
            assert!(!english.is_empty(), "empty key");
            assert!(!russian.is_empty(), "empty translation for {english:?}");
            assert!(keys.insert(*english), "duplicate key {english:?}");
            assert!(
                english.chars().any(|c| c.is_ascii_alphabetic()),
                "key has no Latin letters: {english:?}"
            );
        }
    }

    #[test]
    fn templates_keep_their_placeholders() {
        for (english, russian) in translations() {
            assert_eq!(
                placeholders(english),
                placeholders(russian),
                "placeholder mismatch for {english:?} -> {russian:?}"
            );
        }
    }

    #[test]
    fn language_round_trips_through_serde() {
        for language in Language::ALL {
            let json = serde_json::to_string(&language).unwrap();
            let back: Language = serde_json::from_str(&json).unwrap();
            assert_eq!(back, language);
        }
        assert_eq!(Language::default(), Language::English);
    }

    #[test]
    fn the_template_macro_substitutes_every_named_value() {
        let current = 2;
        let total = 6;
        let text = tr!(
            Language::Russian,
            "Step {current} of {total}",
            { current = current, total = total }
        );
        assert_eq!(text, "Шаг 2 из 6");
        let text = tr!(
            Language::English,
            "Step {current} of {total}",
            { current = 1, total = 6 }
        );
        assert_eq!(text, "Step 1 of 6");
    }
}
