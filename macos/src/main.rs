//! nrzMail als native macOS-App – komplett in Rust, im Stil von nrzX: durchsichtiges Fenster mit
//! dem Milchglas von macOS dahinter, Titelleiste in der Oberfläche (Ampel über der Seitenleiste).
//!
//! * **Mit Server** (empfohlen): Das Fenster zeigt die Oberfläche einer nrzMail-Instanz (z. B. auf
//!   dem Umbrel). Postfächer, Mails, KI und Einstellungen kommen vom Server – nichts muss in der App
//!   neu verbunden werden; die Anmeldung merkt sich WebKit. Ist der Server nicht erreichbar, bietet
//!   die App an, lokal weiterzuarbeiten, und versucht es selbst wieder.
//! * **Lokal**: Ein eigener nrzMail-Server läuft im selben Prozess auf `127.0.0.1` (Daten in
//!   `~/Library/Application Support/nrzMail/local`, Protokoll in `~/Library/Logs/nrzMail`). Die
//!   App meldet sich dort mit einem Token an, das nur sie kennt (`/__desktop/login`) – kein
//!   Passwort, kein Einrichten. So geht nrzMail auch ganz ohne Server.
//!
//! * **Updates**: Die App fragt die Releases des Repositorys in nrzGit ab (`v<version>` mit DMG),
//!   zeigt unten links „Update installieren“, lädt die DMG, ersetzt das Bundle und startet neu
//!   (siehe `updater`).
//!
//! Umschalten über das Menü „nrzMail“ (Mit Server verbinden …, Server verwenden, Lokal verwenden).
//! Einstellungen der App: `settings` (`app.json`).

mod lights;
mod overlay;
mod server;
mod settings;
mod updater;

use muda::{AboutMetadata, CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use overlay::Page;
use settings::{Mode, Settings};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::window::{Window, WindowBuilder, WindowId};
use wry::{BackgroundThrottlingPolicy, NewWindowResponse, PageLoadEvent, WebView, WebViewBuilder};

#[derive(Debug)]
enum UserEvent {
    Menu(MenuEvent),
    LocalReady(Result<u16, String>),
    /// Ergebnis einer Prüfung des Servers (`/api/v1/health`)
    Probed { base: String, result: Result<String, String>, why: Probe },
    /// Nachricht aus einer Seite; `trusted` = von der App selbst, dem lokalen oder dem verbundenen Server
    Ipc { msg: String, trusted: bool, window: WindowId },
    PageLoaded(WindowId),
    OpenWindow(String),
    /// Das Hauptfenster hat eine fremde Seite geladen: im Browser öffnen, Oberfläche zurückholen
    Escaped(String),
    WebContentTerminated(WindowId),
    /// Ergebnis einer Update-Prüfung; `manual` = über das Menü angestossen
    UpdateChecked { result: Result<Option<updater::Update>, String>, manual: bool },
    Progress(f64),
    Downloaded(Result<PathBuf, String>),
}

/// Zustand des Updaters.
#[derive(Default)]
struct UpdateState {
    available: Option<updater::Update>,
    busy: bool,
    /// Version, zu der schon eine Mitteilung kam
    notified: Option<semver::Version>,
}

/// Warum der Server geprüft wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Probe {
    /// Neue Adresse auf der Seite „Verbinden“
    Connect,
    /// Beim Start, über das Menü oder „Erneut versuchen“
    Open,
    /// Automatischer Versuch auf der Seite „nicht erreichbar“ (ohne Hinweis bei Misserfolg)
    Quiet,
}

/// Was das Hauptfenster gerade zeigt.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Showing {
    AppPage,
    Local,
    Server,
}

/// Ursprünge, deren Seiten das Hauptfenster zeigen und die App steuern dürfen.
#[derive(Default)]
struct Origins {
    local: Option<String>,
    server: Option<String>,
}

impl Origins {
    fn own(&self, url: &str) -> bool {
        let Some(o) = settings::origin(url) else { return false };
        self.local.as_deref() == Some(o.as_str()) || self.server.as_deref() == Some(o.as_str())
    }

    /// Darf das Hauptfenster diese Seite zeigen? Eigene Ursprünge und – für Anmeldeseiten davor,
    /// etwa die des Umbrel – derselbe Rechner wie der Server.
    fn allowed(&self, url: &str) -> bool {
        self.own(url) || self.server.as_deref().and_then(host_of).is_some_and(|h| host_of(url).as_deref() == Some(h.as_str()))
    }
}

fn host_of(url: &str) -> Option<String> {
    let o = settings::origin(url)?;
    let rest = o.split_once("://")?.1;
    // [IPv6]:port bzw. host:port
    let host = if rest.starts_with('[') { rest.split(']').next().map(|h| format!("{h}]"))? } else { rest.split(':').next()?.to_string() };
    Some(host)
}

type Shared = Arc<Mutex<Origins>>;

struct MenuIds {
    connect: MenuItem,
    use_server: CheckMenuItem,
    use_local: CheckMenuItem,
    reload: MenuItem,
    check: MenuItem,
    install: MenuItem,
    quit: MenuItem,
}

#[derive(serde::Deserialize)]
struct NotifyMsg {
    title: String,
    #[serde(default)]
    body: String,
}

fn main() {
    init_logging();

    let _lock = match single_instance_lock() {
        Ok(l) => l,
        Err(e) => {
            message("nrzMail", &e, rfd::MessageLevel::Info);
            return;
        }
    };

    updater::cleanup();

    // Umgebung des lokalen Servers vor allen weiteren Threads setzen
    let local_port = server::prepare_env();

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    {
        let p = proxy.clone();
        MenuEvent::set_event_handler(Some(move |e| {
            let _ = p.send_event(UserEvent::Menu(e));
        }));
    }

    let mut settings = Settings::load();
    let (menu, ids) = build_menu();
    refresh_menu(&ids, &settings);

    // Titelleiste in die Oberfläche integriert: transparent, Inhalt bis oben, Milchglas dahinter
    use tao::platform::macos::WindowBuilderExtMacOS;
    let window = WindowBuilder::new()
        .with_title("nrzMail")
        .with_inner_size(tao::dpi::LogicalSize::new(1280.0, 840.0))
        // Schmaler als 768 px zeigt die Oberfläche das Handy-Layout – in der App nie
        .with_min_inner_size(tao::dpi::LogicalSize::new(820.0, 520.0))
        .with_transparent(true)
        .with_titlebar_transparent(true)
        .with_title_hidden(true)
        .with_fullsize_content_view(true)
        .build(&event_loop)
        .expect("Fenster");
    lights::install(&window, false);
    // nrzMail hat ein dunkles Thema – das Milchglas dahinter ebenfalls dunkel
    window.set_theme(Some(tao::window::Theme::Dark));
    let main_id = window.id();

    let origins: Shared = Arc::new(Mutex::new(Origins {
        local: None,
        server: settings.server.as_deref().and_then(settings::origin),
    }));
    let webview = build_webview(&window, &proxy, origins.clone(), true).expect("WebView");
    // Erst jetzt: wry setzt eine eigene Inhaltsansicht ein, das Milchglas gehört darunter
    if !apply_glass(&window) {
        let _ = webview.evaluate_script("window.__nrzGlass=false;document.documentElement.classList.remove('nrz-glass')");
    }

    // Lokaler Server: startet immer (für „lokal“ und als Ausweg, wenn der Server fehlt)
    let local: Arc<Mutex<Option<server::Server>>> = Arc::new(Mutex::new(None));
    match local_port {
        Ok(port) => {
            let p = proxy.clone();
            let slot = local.clone();
            std::thread::spawn(move || {
                let r = server::start(port).map(|s| {
                    let port = s.port;
                    *slot.lock().unwrap() = Some(s);
                    port
                });
                let _ = p.send_event(UserEvent::LocalReady(r));
            });
        }
        Err(e) => {
            let _ = proxy.send_event(UserEvent::LocalReady(Err(e)));
        }
    }

    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("Tokio");
    let probe = {
        let proxy = proxy.clone();
        let handle = rt.handle().clone();
        move |base: String, why: Probe| {
            let p = proxy.clone();
            handle.spawn(async move {
                let result = settings::probe(&base).await;
                let _ = p.send_event(UserEvent::Probed { base, result, why });
            });
        }
    };

    // Updates: kurz nach dem Start, danach alle paar Stunden
    {
        let p = proxy.clone();
        rt.spawn(async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            loop {
                let result = updater::check(&updater::current_version()).await;
                if let Err(e) = &result {
                    tracing::warn!("Update-Prüfung: {e}");
                }
                let _ = p.send_event(UserEvent::UpdateChecked { result, manual: false });
                tokio::time::sleep(updater::CHECK_INTERVAL).await;
            }
        });
    }
    let check_now = {
        let proxy = proxy.clone();
        let handle = rt.handle().clone();
        move || {
            let p = proxy.clone();
            handle.spawn(async move {
                let result = updater::check(&updater::current_version()).await;
                let _ = p.send_event(UserEvent::UpdateChecked { result, manual: true });
            });
        }
    };
    let mut updates = UpdateState::default();

    // Erste Seite je nach Modus
    let mut showing = Showing::AppPage;
    // „Lokal“ gewählt, bevor der lokale Server bereit war
    let mut want_local = false;
    match (settings.mode, settings.server.clone()) {
        (Mode::Server, Some(base)) => {
            let _ = webview.load_html(&overlay::page(Page::Loading(&format!("Verbinde mit {base} …"))));
            probe(base, Probe::Open);
        }
        (Mode::Local, _) => {
            want_local = true;
            let _ = webview.load_html(&overlay::page(Page::Loading("nrzMail startet …")));
        }
        _ => {
            let _ = webview.load_html(&overlay::page(Page::Connect { current: settings.server.as_deref(), error: None, first: true }));
        }
    }

    let mut local_port: Option<u16> = None;
    let mut local_error: Option<String> = None;
    let mut extra: HashMap<WindowId, (Window, WebView)> = HashMap::new();
    let mut main_window = Some((window, webview));
    let mut last_escape: Option<Instant> = None;

    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::Wait;
        let quit = |control_flow: &mut ControlFlow| {
            if let Some(s) = local.lock().unwrap().as_mut() {
                s.stop(Duration::from_secs(4));
            }
            *control_flow = ControlFlow::Exit;
        };
        // Seite der App im Hauptfenster zeigen
        let show_page = |main: &Option<(Window, WebView)>, p: Page<'_>| {
            if let Some((_, wv)) = main {
                let _ = wv.load_html(&overlay::page(p));
            }
        };
        match event {
            Event::NewEvents(StartCause::Init) => {
                menu.init_for_nsapp();
            }
            Event::Reopen { .. } => {
                if let Some((w, _)) = &main_window {
                    w.set_visible(true);
                    w.set_focus();
                }
            }
            Event::WindowEvent { window_id, event: WindowEvent::CloseRequested, .. } => {
                if window_id == main_id {
                    // Wie bei macOS üblich: Fenster weg, App (und Abgleich) laufen weiter
                    if let Some((w, _)) = &main_window {
                        w.set_visible(false);
                    }
                } else {
                    extra.remove(&window_id);
                }
            }
            Event::UserEvent(ev) => match ev {
                UserEvent::LocalReady(Ok(p)) => {
                    local_port = Some(p);
                    origins.lock().unwrap().local = Some(format!("http://127.0.0.1:{p}"));
                    tracing::info!("Lokaler Server auf 127.0.0.1:{p}");
                    if want_local {
                        want_local = false;
                        if let Some((w, wv)) = &main_window {
                            let _ = wv.load_url(&server::login_url(p));
                            w.set_focus();
                        }
                        showing = Showing::Local;
                    }
                }
                UserEvent::LocalReady(Err(e)) => {
                    tracing::error!("Lokaler Server: {e}");
                    if want_local {
                        want_local = false;
                        show_page(&main_window, Page::Failed(&e));
                    }
                    local_error = Some(e);
                }
                UserEvent::Probed { base, result, why } => {
                    // Antwort zu einer Adresse, die inzwischen nicht mehr gilt (anderer Server, lokal)
                    let current = why == Probe::Connect || (settings.mode == Mode::Server && settings.server.as_deref() == Some(base.as_str()));
                    if !current || (why == Probe::Quiet && showing != Showing::AppPage) {
                        return;
                    }
                    match result {
                        Ok(version) => {
                            tracing::info!("Verbunden mit {base} (nrzMail {version})");
                            settings.mode = Mode::Server;
                            settings.server = Some(base.clone());
                            settings.save();
                            refresh_menu(&ids, &settings);
                            origins.lock().unwrap().server = settings::origin(&base);
                            want_local = false;
                            if let Some((w, wv)) = &main_window {
                                let _ = wv.load_url(&format!("{base}/"));
                                w.set_focus();
                            }
                            showing = Showing::Server;
                        }
                        Err(e) => {
                            tracing::warn!("Server {base}: {e}");
                            match why {
                                Probe::Connect => {
                                    if let Some((_, wv)) = &main_window {
                                        let _ = wv.evaluate_script(&format!(
                                            "window.__nrzConnectState && window.__nrzConnectState('error', {})",
                                            overlay::js_str(&e)
                                        ));
                                    }
                                }
                                Probe::Open => {
                                    show_page(&main_window, Page::Offline { server: &base, error: &e });
                                    showing = Showing::AppPage;
                                }
                                Probe::Quiet => {
                                    if let Some((_, wv)) = &main_window {
                                        let _ = wv.evaluate_script("window.__nrzConnectState && window.__nrzConnectState('idle', '')");
                                    }
                                }
                            }
                        }
                    }
                }
                UserEvent::PageLoaded(id) => {
                    if id == main_id {
                        if let Some((w, wv)) = &main_window {
                            send_lights(w, wv);
                        }
                    }
                }
                UserEvent::WebContentTerminated(id) => {
                    tracing::warn!("Inhaltsprozess beendet – lade neu");
                    let all = main_window.iter().chain(extra.values());
                    if let Some((_, wv)) = all.into_iter().find(|(w, _)| w.id() == id) {
                        let _ = wv.reload();
                    }
                }
                UserEvent::Ipc { msg, trusted, window } => {
                    if !trusted || window != main_id {
                        tracing::warn!("Nachricht aus fremder Seite ignoriert");
                        return;
                    }
                    if msg == "update:install" {
                        start_download(&rt, &proxy, &mut updates, &main_window);
                    } else if msg == "update:state" {
                        if let (Some((_, wv)), Some(u), false) = (&main_window, &updates.available, updates.busy) {
                            update_ui(wv, "available", &u.version.to_string());
                        }
                    } else if msg == "update:check" {
                        check_now();
                    } else if msg == "window:drag" {
                        if let Some((w, _)) = &main_window {
                            let _ = w.drag_window();
                        }
                    } else if msg == "window:zoom" {
                        if let Some((w, _)) = &main_window {
                            w.set_maximized(!w.is_maximized());
                        }
                    } else if let Some(json) = msg.strip_prefix("notify:") {
                        if let Ok(n) = serde_json::from_str::<NotifyMsg>(json) {
                            native_notify(&n.title, &n.body);
                        }
                    } else if let Some(input) = msg.strip_prefix("connect:") {
                        if input == "show" {
                            show_page(&main_window, Page::Connect { current: settings.server.as_deref(), error: None, first: false });
                            showing = Showing::AppPage;
                        } else if showing == Showing::AppPage {
                            match settings::normalize(input) {
                                Ok(base) => probe(base, Probe::Connect),
                                Err(e) => {
                                    if let Some((_, wv)) = &main_window {
                                        let _ = wv.evaluate_script(&format!("window.__nrzConnectState('error', {})", overlay::js_str(&e)));
                                    }
                                }
                            }
                        }
                    } else if msg == "retry" || msg == "retry:quiet" {
                        if let (Mode::Server, Some(base), Showing::AppPage) = (settings.mode, settings.server.clone(), &showing) {
                            probe(base, if msg == "retry" { Probe::Open } else { Probe::Quiet });
                        }
                    } else if msg == "local" {
                        use_local(&mut settings, &ids, &main_window, local_port, local_error.as_deref(), &mut want_local, &mut showing);
                    }
                }
                UserEvent::Escaped(url) => {
                    tracing::warn!("Hauptfenster verliess die App: {url} – im Browser geöffnet");
                    open_external(&url);
                    // Immer wieder hinaus (Weiterleitung im Kreis): Seite „Verbinden“ statt Schleife
                    let looping = last_escape.is_some_and(|t| t.elapsed() < Duration::from_secs(5));
                    last_escape = Some(Instant::now());
                    if looping {
                        show_page(&main_window, Page::Connect { current: settings.server.as_deref(), error: Some("Der Server leitet auf eine fremde Seite weiter."), first: false });
                        showing = Showing::AppPage;
                    } else if let Some((_, wv)) = &main_window {
                        match (&showing, local_port, settings.server.as_deref()) {
                            (Showing::Local, Some(p), _) => {
                                let _ = wv.load_url(&server::login_url(p));
                            }
                            (Showing::Server, _, Some(base)) => {
                                let _ = wv.load_url(&format!("{base}/"));
                            }
                            _ => {}
                        }
                    }
                }
                UserEvent::UpdateChecked { result, manual } => match result {
                    Ok(Some(u)) => {
                        ids.install.set_text(format!("Update auf {} installieren …", u.version));
                        ids.install.set_enabled(true);
                        if !updates.busy {
                            update_main(&main_window, "available", &u.version.to_string());
                        }
                        if manual {
                            if let Some((w, _)) = &main_window {
                                w.set_visible(true);
                                w.set_focus();
                            }
                        } else if updates.notified.as_ref() != Some(&u.version) {
                            native_notify("Update verfügbar", &format!("nrzMail {} ist da – unten links im Fenster installieren.", u.version));
                            updates.notified = Some(u.version.clone());
                        }
                        updates.available = Some(u);
                        // NRZMAIL_AUTO_UPDATE=1: ohne Rückfrage laden und installieren
                        if std::env::var("NRZMAIL_AUTO_UPDATE").is_ok_and(|v| v == "1") {
                            start_download(&rt, &proxy, &mut updates, &main_window);
                        }
                    }
                    Ok(None) => {
                        if manual {
                            update_main(&main_window, "current", &updater::current_version().to_string());
                            message("Keine Updates", &format!("nrzMail {} ist die neueste Version.", updater::current_version()), rfd::MessageLevel::Info);
                        }
                    }
                    Err(e) => {
                        if manual {
                            message("Update-Prüfung fehlgeschlagen", &e, rfd::MessageLevel::Warning);
                        }
                    }
                },
                UserEvent::Progress(p) => update_main(&main_window, "progress", &p.to_string()),
                UserEvent::Downloaded(Err(e)) => {
                    updates.busy = false;
                    tracing::error!("Update: {e}");
                    update_main(&main_window, "failed", &e);
                }
                UserEvent::Downloaded(Ok(dmg)) => {
                    update_main(&main_window, "installing", "");
                    match updater::install(&dmg) {
                        Ok(updater::Installed::Replaced(bundle)) => {
                            tracing::info!("Update installiert, Neustart");
                            updater::relaunch_after_exit(&bundle);
                            quit(control_flow);
                        }
                        Ok(updater::Installed::OpenedDmg(text)) => {
                            updates.busy = false;
                            if let Some(u) = &updates.available {
                                update_main(&main_window, "available", &u.version.to_string());
                            }
                            message("Update bereit", &text, rfd::MessageLevel::Info);
                        }
                        Err(e) => {
                            updates.busy = false;
                            update_main(&main_window, "failed", &e);
                        }
                    }
                }
                UserEvent::OpenWindow(url) => {
                    open_window(target, &proxy, &origins, &mut extra, &url);
                }
                UserEvent::Menu(e) => {
                    let id = e.id();
                    if id == ids.quit.id() {
                        quit(control_flow);
                    } else if id == ids.connect.id() {
                        if let Some((w, _)) = &main_window {
                            w.set_visible(true);
                            w.set_focus();
                        }
                        show_page(&main_window, Page::Connect { current: settings.server.as_deref(), error: None, first: false });
                        showing = Showing::AppPage;
                    } else if id == ids.use_server.id() {
                        if let Some(base) = settings.server.clone() {
                            settings.mode = Mode::Server;
                            settings.save();
                            show_page(&main_window, Page::Loading(&format!("Verbinde mit {base} …")));
                            showing = Showing::AppPage;
                            probe(base, Probe::Open);
                        }
                        refresh_menu(&ids, &settings);
                    } else if id == ids.use_local.id() {
                        use_local(&mut settings, &ids, &main_window, local_port, local_error.as_deref(), &mut want_local, &mut showing);
                    } else if id == ids.check.id() {
                        check_now();
                    } else if id == ids.install.id() {
                        start_download(&rt, &proxy, &mut updates, &main_window);
                    } else if id == ids.reload.id() {
                        match (&showing, settings.server.clone()) {
                            (Showing::AppPage, Some(base)) if settings.mode == Mode::Server => probe(base, Probe::Open),
                            (Showing::AppPage, _) => {}
                            _ => {
                                if let Some((_, wv)) = &main_window {
                                    let _ = wv.reload();
                                }
                            }
                        }
                    }
                }
            },
            Event::LoopDestroyed => {
                main_window.take();
                if let Some(s) = local.lock().unwrap().as_mut() {
                    s.stop(Duration::from_secs(4));
                }
            }
            _ => {}
        }
    });
}

/// Auf den lokalen Server umschalten (sobald er bereit ist).
fn use_local(
    settings: &mut Settings,
    ids: &MenuIds,
    main: &Option<(Window, WebView)>,
    port: Option<u16>,
    error: Option<&str>,
    want_local: &mut bool,
    showing: &mut Showing,
) {
    settings.mode = Mode::Local;
    settings.save();
    refresh_menu(ids, settings);
    let Some((_, wv)) = main else { return };
    match (port, error) {
        (Some(p), _) => {
            let _ = wv.load_url(&server::login_url(p));
            *showing = Showing::Local;
        }
        (None, Some(e)) => {
            let _ = wv.load_html(&overlay::page(Page::Failed(e)));
            *showing = Showing::AppPage;
        }
        (None, None) => {
            *want_local = true;
            let _ = wv.load_html(&overlay::page(Page::Loading("nrzMail startet …")));
            *showing = Showing::AppPage;
        }
    }
}

fn build_webview(window: &Window, proxy: &EventLoopProxy<UserEvent>, origins: Shared, is_main: bool) -> wry::Result<WebView> {
    build_webview_at(window, proxy, origins, is_main, None)
}

fn build_webview_at(window: &Window, proxy: &EventLoopProxy<UserEvent>, origins: Shared, is_main: bool, url: Option<&str>) -> wry::Result<WebView> {
    let id = window.id();
    let p_ipc = proxy.clone();
    let p_load = proxy.clone();
    let p_new = proxy.clone();
    let p_term = proxy.clone();
    let o_ipc = origins.clone();
    let o_load = origins.clone();
    let o_new = origins;
    let devtools = cfg!(debug_assertions) || std::env::var("NRZMAIL_DEVTOOLS").is_ok_and(|v| v == "1");
    use wry::WebViewBuilderExtDarwin;
    let builder = WebViewBuilder::new()
        .with_initialization_script_for_main_only(if is_main { overlay::WINDOW_SCRIPT } else { "" }, true)
        .with_initialization_script_for_main_only(if is_main { overlay::GLASS_SCRIPT } else { "" }, true)
        .with_initialization_script_for_main_only(if is_main { overlay::NATIVE_SCRIPT } else { "" }, true)
        .with_initialization_script_for_main_only(if is_main { overlay::UPDATE_SCRIPT } else { "" }, true)
        .with_transparent(is_main)
        .with_ipc_handler(move |req| {
            // Nur Seiten der App (about:), des lokalen und des verbundenen Servers steuern die App
            let uri = req.uri().to_string();
            let app_page = uri.is_empty() || uri.starts_with("about:");
            let trusted = app_page || o_ipc.lock().unwrap().own(&uri);
            let _ = p_ipc.send_event(UserEvent::Ipc { msg: req.body().clone(), trusted, window: id });
        })
        .with_on_page_load_handler(move |ev, url| {
            if is_main && is_web(&url) && !o_load.lock().unwrap().allowed(&url) {
                let _ = p_load.send_event(UserEvent::Escaped(url));
                return;
            }
            if matches!(ev, PageLoadEvent::Finished) {
                let _ = p_load.send_event(UserEvent::PageLoaded(id));
            }
        })
        .with_new_window_req_handler(move |url, _features| {
            // Seiten des eigenen Servers (Anhänge, PDF) in einem App-Fenster, alles andere im Browser
            if o_new.lock().unwrap().own(&url) {
                let _ = p_new.send_event(UserEvent::OpenWindow(url));
            } else {
                open_external(&url);
            }
            NewWindowResponse::Deny
        })
        .with_download_started_handler(|url, path| {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .filter(|n| !n.is_empty())
                .or_else(|| url.rsplit('/').next().map(|s| s.split('?').next().unwrap_or(s).to_string()))
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "download".into());
            *path = unique_path(server::home().join("Downloads").join(name));
            true
        })
        .with_download_completed_handler(|_url, path, ok| {
            if let (Some(p), true) = (path, ok) {
                reveal(&p);
            }
        })
        .with_clipboard(true)
        .with_devtools(devtools)
        .with_back_forward_navigation_gestures(false)
        // Im Hintergrund nicht drosseln: sonst lädt WebKit eine verborgene Oberfläche nach einigen
        // Minuten neu (offene Mail, Entwurf weg)
        .with_background_throttling(if is_main { BackgroundThrottlingPolicy::Disabled } else { BackgroundThrottlingPolicy::Suspend })
        .with_on_web_content_process_terminate_handler(move || {
            let _ = p_term.send_event(UserEvent::WebContentTerminated(id));
        });
    let builder = match url {
        Some(u) => builder.with_url(u),
        None => builder.with_html(overlay::page(Page::Loading("nrzMail startet …"))),
    };
    builder.build(window)
}

fn open_window(target: &EventLoopWindowTarget<UserEvent>, proxy: &EventLoopProxy<UserEvent>, origins: &Shared, extra: &mut HashMap<WindowId, (Window, WebView)>, url: &str) {
    let Ok(window) = WindowBuilder::new().with_title("nrzMail").with_inner_size(tao::dpi::LogicalSize::new(1000.0, 760.0)).build(target) else {
        return;
    };
    window.set_theme(Some(tao::window::Theme::Dark));
    match build_webview_at(&window, proxy, origins.clone(), false, Some(url)) {
        Ok(wv) => {
            extra.insert(window.id(), (window, wv));
        }
        Err(e) => tracing::error!("Fenster: {e}"),
    }
}

fn is_web(url: &str) -> bool {
    let u = url.to_ascii_lowercase();
    u.starts_with("http://") || u.starts_with("https://")
}

/// Lage der Ampel an die Oberfläche (`--nrz-tl-y` Mitte, `--nrz-tl-r` rechter Rand).
fn send_lights(window: &Window, wv: &WebView) {
    if let Some((y, r)) = lights::spot(window) {
        let _ = wv.evaluate_script(&format!(
            "document.documentElement.style.setProperty('--nrz-tl-y','{y:.1}px');document.documentElement.style.setProperty('--nrz-tl-r','{r:.1}px')"
        ));
    }
}

/// Milchglas (Vibrancy wie die Seitenleisten nativer Apps) unter dem WebView.
fn apply_glass(window: &Window) -> bool {
    let result = match content_view(window) {
        Some(view) => window_vibrancy::apply_vibrancy(
            view,
            window_vibrancy::NSVisualEffectMaterial::UnderWindowBackground,
            Some(window_vibrancy::NSVisualEffectState::Active),
            None,
        )
        .map_err(|e| format!("{e:?}")),
        None => Err("keine Inhaltsansicht".to_string()),
    };
    match result {
        Ok(()) => true,
        Err(e) => {
            tracing::info!("Kein Milchglas: {e}");
            false
        }
    }
}

/// Aktuelle Inhaltsansicht des Fensters: wry ersetzt die von tao durch eine eigene mit dem
/// WebView darin – Vibrancy an der alten bliebe unsichtbar.
struct ContentView(std::ptr::NonNull<std::ffi::c_void>);

impl tao::rwh_06::HasWindowHandle for ContentView {
    fn window_handle(&self) -> Result<tao::rwh_06::WindowHandle<'_>, tao::rwh_06::HandleError> {
        let raw = tao::rwh_06::RawWindowHandle::AppKit(tao::rwh_06::AppKitWindowHandle::new(self.0));
        // SAFETY: Das Fenster hält seine Inhaltsansicht, solange es offen ist; das Handle wird nur
        // sofort (im Hauptthread) benutzt.
        Ok(unsafe { tao::rwh_06::WindowHandle::borrow_raw(raw) })
    }
}

fn content_view(window: &Window) -> Option<ContentView> {
    use objc2_app_kit::NSWindow;
    use tao::platform::macos::WindowExtMacOS;
    // SAFETY: NSWindow des lebenden tao-Fensters, Aufruf im Hauptthread.
    let ns = unsafe { (window.ns_window() as *const NSWindow).as_ref()? };
    let view = ns.contentView()?;
    std::ptr::NonNull::new(objc2::rc::Retained::as_ptr(&view) as *mut std::ffi::c_void).map(ContentView)
}

/// Stand des Updates an den Hinweis unten links (`overlay::UPDATE_SCRIPT`).
fn update_ui(wv: &WebView, status: &str, text: &str) {
    let _ = wv.evaluate_script(&format!(
        "window.__nrzUpdateState && window.__nrzUpdateState({}, {})",
        overlay::js_str(status),
        overlay::js_str(text)
    ));
}

fn update_main(main: &Option<(Window, WebView)>, status: &str, text: &str) {
    if let Some((_, wv)) = main {
        update_ui(wv, status, text);
    }
}

/// DMG laden (Fortschritt in ganzen Prozenten an den Hinweis).
fn start_download(rt: &tokio::runtime::Runtime, proxy: &EventLoopProxy<UserEvent>, updates: &mut UpdateState, main: &Option<(Window, WebView)>) {
    let Some(u) = updates.available.clone() else { return };
    if updates.busy {
        return;
    }
    updates.busy = true;
    if let Some((w, wv)) = main {
        w.set_visible(true);
        update_ui(wv, "progress", "0");
    }
    let p = proxy.clone();
    rt.spawn(async move {
        let pp = p.clone();
        let last = Mutex::new(-1i64);
        let r = updater::download(&u, move |f| {
            let step = if f < 0.0 { -1 } else { (f * 100.0) as i64 };
            let mut l = last.lock().unwrap();
            if *l != step {
                *l = step;
                let _ = pp.send_event(UserEvent::Progress(f));
            }
        })
        .await;
        let _ = p.send_event(UserEvent::Downloaded(r));
    });
}

fn build_menu() -> (Menu, MenuIds) {
    let ids = MenuIds {
        connect: MenuItem::new("Mit Server verbinden …", true, Some("CmdOrCtrl+Shift+K".parse().unwrap())),
        use_server: CheckMenuItem::new("Server verwenden", false, false, None),
        use_local: CheckMenuItem::new("Lokal verwenden (ohne Server)", true, false, None),
        reload: MenuItem::new("Neu laden", true, Some("CmdOrCtrl+R".parse().unwrap())),
        check: MenuItem::new("Nach Updates suchen …", true, None),
        install: MenuItem::new("Update installieren …", false, None),
        quit: MenuItem::new("nrzMail beenden", true, Some("CmdOrCtrl+Q".parse().unwrap())),
    };
    let about = AboutMetadata {
        name: Some("nrzMail".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        comments: Some("Mail-Client und Mail-Archiv – komplett in Rust.".into()),
        license: Some("AGPL-3.0-or-later".into()),
        ..Default::default()
    };
    let app = Submenu::with_items(
        "nrzMail",
        true,
        &[
            &PredefinedMenuItem::about(Some("Über nrzMail"), Some(about)),
            &PredefinedMenuItem::separator(),
            &ids.check,
            &ids.install,
            &PredefinedMenuItem::separator(),
            &ids.connect,
            &ids.use_server,
            &ids.use_local,
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(None),
            &PredefinedMenuItem::hide_others(None),
            &PredefinedMenuItem::show_all(None),
            &PredefinedMenuItem::separator(),
            &ids.quit,
        ],
    )
    .unwrap();
    // Ohne diese Einträge gingen ⌘C, ⌘V, ⌘X, ⌘A und ⌘Z im WebView nicht
    let edit = Submenu::with_items(
        "Bearbeiten",
        true,
        &[
            &PredefinedMenuItem::undo(None),
            &PredefinedMenuItem::redo(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::cut(None),
            &PredefinedMenuItem::copy(None),
            &PredefinedMenuItem::paste(None),
            &PredefinedMenuItem::select_all(None),
        ],
    )
    .unwrap();
    let view = Submenu::with_items("Ansicht", true, &[&ids.reload, &PredefinedMenuItem::separator(), &PredefinedMenuItem::fullscreen(None)]).unwrap();
    let win = Submenu::with_items(
        "Fenster",
        true,
        &[&PredefinedMenuItem::minimize(None), &PredefinedMenuItem::maximize(None), &PredefinedMenuItem::separator(), &PredefinedMenuItem::close_window(None)],
    )
    .unwrap();
    let menu = Menu::with_items(&[&app, &edit, &view, &win]).unwrap();
    (menu, ids)
}

/// Häkchen und Texte im Menü nach den Einstellungen.
fn refresh_menu(ids: &MenuIds, s: &Settings) {
    match s.server.as_deref() {
        Some(base) => {
            let host = base.split_once("://").map(|(_, h)| h).unwrap_or(base);
            ids.use_server.set_text(format!("Server verwenden ({host})"));
            ids.use_server.set_enabled(true);
        }
        None => {
            ids.use_server.set_text("Server verwenden");
            ids.use_server.set_enabled(false);
        }
    }
    ids.use_server.set_checked(s.mode == Mode::Server);
    ids.use_local.set_checked(s.mode == Mode::Local);
}

/// Mitteilung von macOS (im Mitteilungszentrum, mit Ton).
fn native_notify(title: &str, body: &str) {
    #[allow(deprecated)]
    {
        use objc2_foundation::{NSString, NSUserNotification, NSUserNotificationCenter, NSUserNotificationDefaultSoundName};
        let title: String = title.chars().take(120).collect();
        let body: String = body.chars().take(400).collect();
        // SAFETY: Aufruf im Hauptthread (Event-Loop); deprecated, aber ohne Signatur nutzbar.
        unsafe {
            let n = NSUserNotification::new();
            n.setTitle(Some(&NSString::from_str(&title)));
            n.setInformativeText(Some(&NSString::from_str(&body)));
            n.setSoundName(Some(NSUserNotificationDefaultSoundName));
            NSUserNotificationCenter::defaultUserNotificationCenter().deliverNotification(&n);
        }
    }
}

fn open_external(target: &str) {
    let _ = std::process::Command::new("/usr/bin/open").arg(target).spawn();
}

fn reveal(path: &std::path::Path) {
    let _ = std::process::Command::new("/usr/bin/open").arg("-R").arg(path).spawn();
}

fn message(title: &str, text: &str, level: rfd::MessageLevel) {
    rfd::MessageDialog::new().set_title(title).set_description(text).set_level(level).set_buttons(rfd::MessageButtons::Ok).show();
}

fn unique_path(p: PathBuf) -> PathBuf {
    if !p.exists() {
        return p;
    }
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let dir = p.parent().map(PathBuf::from).unwrap_or_default();
    (2..1000).map(|i| dir.join(format!("{stem} {i}{ext}"))).find(|c| !c.exists()).unwrap_or(p)
}

/// Nur eine Instanz: zwei lokale Server auf derselben Datenbank vertragen sich nicht.
fn single_instance_lock() -> Result<std::fs::File, String> {
    let dir = server::data_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("app.lock")).map_err(|e| e.to_string())?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(_) => Err("nrzMail läuft bereits.".into()),
    }
}

/// Protokoll nach `~/Library/Logs/nrzMail/nrzmail.log`.
fn init_logging() {
    use tracing_subscriber::EnvFilter;
    let dir = server::log_dir();
    let _ = std::fs::create_dir_all(&dir);
    let filter = || {
        EnvFilter::try_from_env("NRZMAIL_LOG").unwrap_or_else(|_| EnvFilter::new("info,rocket::server=warn,rocket::launch::_=warn,rocket::shield=warn"))
    };
    let path = dir.join("nrzmail.log");
    if std::fs::metadata(&path).map(|m| m.len() > 10 * 1024 * 1024).unwrap_or(false) {
        let _ = std::fs::rename(&path, dir.join("nrzmail.old.log"));
    }
    match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        Ok(file) => tracing_subscriber::fmt().with_ansi(false).with_env_filter(filter()).with_writer(Mutex::new(file)).init(),
        Err(_) => tracing_subscriber::fmt().with_env_filter(filter()).init(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_allow() {
        let o = Origins { local: Some("http://127.0.0.1:18330".into()), server: Some("http://umbrel.local:8330".into()) };
        assert!(o.own("http://127.0.0.1:18330/mail"));
        assert!(o.own("http://umbrel.local:8330/"));
        assert!(!o.own("http://umbrel.local/login"));
        assert!(o.allowed("http://umbrel.local/login"));
        assert!(!o.allowed("https://evil.example/"));
        assert!(!o.allowed("http://127.0.0.1:9999/"));
    }
}
