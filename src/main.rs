//! LC Menubar: Laravel Cloud deployments, metrics and spend in the macOS menu bar.

mod api;
mod changelog;
mod charts;
mod menu_views;
mod model;
mod poller;
mod settings;
mod system;
mod updater;
mod vars;
mod windows;

use chrono::{Local, Utc};
use menu_views::{Card, Tone};
use model::{
    Environment, Light, Metrics, Period, Phase, Snapshot, ago, first_line, fmt_count, fmt_pct,
    truncate,
};
use poller::{Command, Credentials};
use serde_json::{Value, json};
use settings::{Account, Settings, Shared};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::window::WindowId;
use tokio::sync::mpsc::UnboundedSender;
use tray_icon::menu::{
    ContextMenu, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
    accelerator::Accelerator,
};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use windows::WebWindow;

const CLOUD_URL: &str = "https://cloud.laravel.com";
const ENV_ACCOUNT: &str = "env";

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum WinKey {
    Settings,
    Vars(String),
    Logs(String),
    Commands(String),
    /// The window offering a new version.
    Update,
}

enum UserEvent {
    Snapshot(Snapshot),
    Menu(MenuEvent),
    Ipc {
        key: WinKey,
        msg: String,
    },
    /// Call `window.<func>(arg)` in a window, if it is still open.
    ToWindow {
        key: WinKey,
        func: &'static str,
        arg: Value,
    },
    AccountAdded {
        id: String,
        name: String,
        token: String,
    },
    /// Development (`LCM_POPUP`): open the menu or an environment submenu.
    DebugPopup(Option<usize>),
    DebugWindow(String),
    /// The range control in an open menu changed.
    SetPeriod(Period),
    /// The organization switch changed (0 = all).
    SetOrg(usize),
    /// Result of an update check; `manual` checks report "up to date" too.
    UpdateChecked {
        result: Result<Option<updater::Release>, String>,
        manual: bool,
    },
    /// An update was installed; quit so the new version can start.
    UpdateInstalled(std::path::PathBuf),
    UpdateFailed(String),
    /// Create the hidden warm-up web view.
    Warm,
    /// Show a window that has not reported `ready` in time.
    ShowWindow(WinKey),
    /// Open a window from code that has no event loop target at hand.
    OpenWindow(WinKey),
}

#[derive(Clone, Debug)]
enum EnvAction {
    Deploy,
    /// Point the environment at a branch and deploy it (`None`: ask for a name).
    SwitchBranch(Option<String>),
    Start,
    Stop,
    Purge,
}

#[derive(Clone, Debug)]
enum Action {
    Refresh,
    OpenUrl(String),
    Copy(String),
    Env { env_id: String, action: EnvAction },
    Window(WinKey),
    Update,
    About,
    Quit,
}

struct App {
    proxy: EventLoopProxy<UserEvent>,
    poller: UnboundedSender<Command>,
    settings: Shared,
    /// account id → token
    tokens: Arc<RwLock<HashMap<String, String>>>,
    jobs: tokio::runtime::Runtime,
    tray: Option<TrayIcon>,
    actions: HashMap<MenuId, Action>,
    snapshot: Snapshot,
    loading: bool,
    windows: HashMap<WinKey, WebWindow>,
    /// Top-level indexes of environment rows, for the development popup.
    row_paths: Vec<usize>,
    menu: Option<Menu>,
    /// Paths of the current menu's cards and rows, to tell whether a new build
    /// can be applied in place.
    layout: Vec<String>,
    demo: bool,
    /// Hidden web view that keeps WebKit warm (see `WebWindow::warm`).
    warm: Option<windows::WebWindow>,
    /// When the last window started opening (`LCM_TIMING`).
    window_opened_at: Option<std::time::Instant>,
    /// Account ids behind the organization switch segments (index 0 = all).
    org_ids: Vec<Option<String>>,
    /// A newer release, offered at the bottom of the menu.
    update: Option<updater::Release>,
    installing: bool,
}

fn main() {
    if let Some(dir) = std::env::args()
        .skip_while(|a| a != "--preview-cards")
        .nth(1)
    {
        return preview_cards(&dir);
    }
    // `--app-icon <dir>`: write the app icon's iconset PNGs (for packaging).
    if let Some(dir) = std::env::args().skip_while(|a| a != "--app-icon").nth(1) {
        return write_iconset(&dir);
    }
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
    }

    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = proxy.send_event(UserEvent::Menu(e));
    }));

    // `--demo`: sample data, no Keychain or network (for UI work).
    let demo = std::env::args().any(|a| a == "--demo");
    let mut loaded = settings::load();
    settings::migrate_login_item();
    loaded.start_at_login = settings::login_item_installed();
    let tokens = if demo {
        HashMap::new()
    } else {
        load_tokens(&mut loaded)
    };
    let settings: Shared = Arc::new(RwLock::new(loaded));
    let tokens = Arc::new(RwLock::new(tokens));

    let proxy = event_loop.create_proxy();
    let poller = if demo {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        std::mem::forget(rx);
        let period = settings.read().unwrap().metrics_period;
        let _ = proxy.send_event(UserEvent::Snapshot(demo_snapshot(period)));
        tx
    } else {
        poller::spawn(
            credentials(&settings, &tokens),
            settings.clone(),
            move |s| {
                let _ = proxy.send_event(UserEvent::Snapshot(s));
            },
        )
    };

    let jobs = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");

    let mut app = App {
        proxy: event_loop.create_proxy(),
        poller,
        settings,
        tokens,
        jobs,
        tray: None,
        actions: HashMap::new(),
        snapshot: Snapshot::default(),
        loading: true,
        windows: HashMap::new(),
        row_paths: vec![],
        menu: None,
        layout: vec![],
        demo,
        window_opened_at: None,
        warm: None,
        org_ids: vec![],
        update: None,
        installing: false,
    };
    if !demo {
        app.schedule_update_checks();
    }
    if let Ok(seq) = std::env::var("LCM_TEST_PERIOD") {
        // Development: change the range (e.g. "24h,1h") after a popup opened, to
        // check that an open menu updates in place, and how fast.
        let periods: Vec<Period> = seq
            .split(',')
            .filter_map(|v| PERIODS.into_iter().find(|p| p.as_str() == v.trim()))
            .collect();
        let proxy = event_loop.create_proxy();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(3500));
            for p in periods {
                let _ = proxy.send_event(UserEvent::SetPeriod(p));
                std::thread::sleep(std::time::Duration::from_millis(800));
            }
        });
    }
    {
        // Warm WebKit up once the menu bar item is in place.
        let proxy = event_loop.create_proxy();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(1000));
            let _ = proxy.send_event(UserEvent::Warm);
        });
    }
    if let Ok(which) = std::env::var("LCM_POPUP") {
        // e.g. LCM_POPUP=menu or LCM_POPUP=0 (first environment's submenu)
        let proxy = event_loop.create_proxy();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(1500));
            let _ = proxy.send_event(match which.as_str() {
                "settings" | "logs" | "commands" | "vars" | "update" => {
                    UserEvent::DebugWindow(which)
                }
                _ => UserEvent::DebugPopup(which.parse().ok()),
            });
        });
    }

    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(StartCause::Init) => {
                install_edit_menu();
                #[cfg(target_os = "macos")]
                {
                    let proxy = app.proxy.clone();
                    menu_views::set_range_handler(move |kind, i| {
                        let _ = proxy.send_event(match kind {
                            menu_views::Segment::Range => match PERIODS.get(i) {
                                Some(p) => UserEvent::SetPeriod(*p),
                                None => return,
                            },
                            menu_views::Segment::Org => UserEvent::SetOrg(i),
                        });
                    });
                }
                #[cfg(target_os = "macos")]
                if let Ok(a) = std::env::var("LCM_APPEARANCE") {
                    menu_views::force_appearance(a == "dark");
                }
                app.tray = Some(
                    TrayIconBuilder::new()
                        .with_title("")
                        .with_tooltip("Laravel Cloud")
                        .build()
                        .expect("tray icon"),
                );
                app.rerender();
                if !demo && app.tokens.read().unwrap().is_empty() {
                    app.open(WinKey::Settings, target);
                }
            }
            Event::UserEvent(UserEvent::Snapshot(s)) => {
                app.snapshot = s;
                app.snapshot.hidden = app.settings().hidden_in(&app.snapshot.apps);
                app.loading = false;
                app.sync_account_names();
                app.rerender();
            }
            Event::UserEvent(UserEvent::Menu(e)) => {
                if let Some(action) = app.actions.get(e.id()).cloned() {
                    if let Action::Quit = action {
                        *control_flow = ControlFlow::Exit;
                    } else {
                        app.handle(action, target);
                    }
                }
            }
            Event::UserEvent(UserEvent::Ipc { key, msg }) => app.on_ipc(key, &msg, target),
            Event::UserEvent(UserEvent::SetPeriod(p)) => app.set_period(p),
            Event::UserEvent(UserEvent::SetOrg(i)) => app.set_org(i),
            Event::UserEvent(UserEvent::UpdateChecked { result, manual }) => {
                app.on_update_checked(result, manual)
            }
            Event::UserEvent(UserEvent::UpdateInstalled(bundle)) => {
                updater::relaunch(&bundle);
                *control_flow = ControlFlow::Exit;
            }
            Event::UserEvent(UserEvent::UpdateFailed(e)) => {
                app.installing = false;
                app.rerender();
                menu_views::alert("Update failed", &e);
            }
            Event::UserEvent(UserEvent::ToWindow { key, func, arg }) => {
                if let Some(w) = app.windows.get(&key) {
                    w.call(func, &arg);
                }
            }
            Event::UserEvent(UserEvent::AccountAdded { id, name, token }) => {
                app.tokens.write().unwrap().insert(id.clone(), token);
                {
                    let mut s = app.settings.write().unwrap();
                    s.accounts.push(Account {
                        id,
                        name: name.clone(),
                    });
                    let _ = settings::save(&s);
                }
                app.push_credentials();
                app.loading = true;
                app.rerender();
                app.refresh_settings_accounts(Some(&format!("Connected {name}")));
            }
            Event::UserEvent(UserEvent::DebugPopup(row)) => {
                #[cfg(target_os = "macos")]
                if let Some(menu) = &app.menu {
                    let path: Vec<usize> = match row {
                        Some(i) => app.row_paths.get(i).map(|p| vec![*p]).unwrap_or_default(),
                        None => vec![],
                    };
                    menu_views::popup(menu.ns_menu(), &path, 1200.0, 1000.0);
                }
                let _ = row;
            }
            Event::UserEvent(UserEvent::Warm) => {
                if app.warm.is_none() {
                    app.warm = windows::WebWindow::warm(target).ok();
                }
            }
            Event::UserEvent(UserEvent::OpenWindow(key)) => app.open(key, target),
            Event::UserEvent(UserEvent::ShowWindow(key)) => {
                if let Some(w) = app.windows.get(&key) {
                    w.show();
                }
            }
            Event::UserEvent(UserEvent::DebugWindow(kind)) => {
                let env = app
                    .snapshot
                    .all_environments()
                    .next()
                    .map(|(_, e)| e.id.clone())
                    .unwrap_or_default();
                let key = match kind.as_str() {
                    "update" => {
                        // A sample release built from this version's changelog.
                        let notes = changelog::releases()
                            .into_iter()
                            .next()
                            .map(|r| {
                                r.groups
                                    .iter()
                                    .map(|(g, items)| {
                                        format!("### {g}\n\n- {}\n", items.join("\n- "))
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            })
                            .unwrap_or_default();
                        app.update = Some(updater::Release {
                            version: "9.9.9".into(),
                            notes,
                            page: "https://github.com/mstaack/lc-menubar/releases".into(),
                            zip_url: None,
                            sha256_url: None,
                        });
                        WinKey::Update
                    }
                    "logs" => WinKey::Logs(env),
                    "commands" => WinKey::Commands(env),
                    "vars" => WinKey::Vars(env),
                    _ => WinKey::Settings,
                };
                app.open(key, target);
            }
            Event::WindowEvent {
                window_id,
                event: WindowEvent::CloseRequested,
                ..
            } => {
                app.close_window(window_id, target);
            }
            _ => {}
        }
    });
}

/// Tokens for all saved accounts, migrating a single-token setup on first run.
fn load_tokens(s: &mut Settings) -> HashMap<String, String> {
    let mut tokens = HashMap::new();
    if s.accounts.is_empty() {
        let id = new_account_id();
        if let Some(t) = system::migrate_legacy_token(&id) {
            s.accounts.push(Account {
                id: id.clone(),
                name: "Laravel Cloud".into(),
            });
            let _ = settings::save(s);
            tokens.insert(id, t);
        }
    }
    for a in &s.accounts {
        if let Some(t) = system::load_token(&a.id) {
            tokens.insert(a.id.clone(), t);
        }
    }
    if let Some(t) = system::env_token() {
        tokens.insert(ENV_ACCOUNT.into(), t);
    }
    tokens
}

fn credentials(settings: &Shared, tokens: &Arc<RwLock<HashMap<String, String>>>) -> Credentials {
    let tokens = tokens.read().unwrap();
    let s = settings.read().unwrap();
    let mut out: Credentials = s
        .accounts
        .iter()
        .filter_map(|a| tokens.get(&a.id).map(|t| (a.id.clone(), t.clone())))
        .collect();
    if let Some(t) = tokens.get(ENV_ACCOUNT) {
        out.push((ENV_ACCOUNT.into(), t.clone()));
    }
    out
}

fn new_account_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("{nanos:x}")
}

impl App {
    fn settings(&self) -> Settings {
        self.settings.read().unwrap().clone()
    }

    fn push_credentials(&self) {
        let _ = self.poller.send(Command::SetAccounts(credentials(
            &self.settings,
            &self.tokens,
        )));
    }

    /// Keep saved account names in step with the organization names the API reports.
    fn sync_account_names(&mut self) {
        let mut changed = false;
        {
            let mut s = self.settings.write().unwrap();
            for app in &self.snapshot.apps {
                if let Some(a) = s.accounts.iter_mut().find(|a| a.id == app.account)
                    && a.name != app.org
                    && !app.org.is_empty()
                {
                    a.name = app.org.clone();
                    changed = true;
                }
            }
            if changed {
                let _ = settings::save(&s);
            }
        }
        if changed {
            self.refresh_settings_accounts(None);
        }
    }

    fn rerender(&mut self) {
        let started = std::time::Instant::now();
        self.rerender_inner();
        if std::env::var_os("LCM_TIMING").is_some() {
            eprintln!(
                "rerender: {:.1} ms",
                started.elapsed().as_secs_f64() * 1000.0
            );
        }
    }

    fn rerender_inner(&mut self) {
        let settings = self.settings();
        let Some(tray) = &self.tray else { return };
        #[cfg(target_os = "macos")]
        charts::set_dark(menu_views::is_dark());
        let t0 = std::time::Instant::now();
        let update = self
            .update
            .as_ref()
            .map(|r| (r.version.as_str(), self.installing));
        let built = build_menu(&self.snapshot, &settings, self.loading, update);
        if std::env::var_os("LCM_TIMING").is_some() {
            eprintln!(
                "  build_menu (incl. chart rendering): {:.1} ms",
                t0.elapsed().as_secs_f64() * 1000.0
            );
        }
        let layout: Vec<String> = built
            .cards
            .iter()
            .map(|(p, _)| format!("{p:?}"))
            .chain(built.rows.iter().map(|(p, _)| format!("{p:?}")))
            .chain(built.signature.iter().cloned())
            .collect();
        // Swapping the menu would leave an open menu showing stale content. When the
        // structure is unchanged, update the current menu's cards and rows in place.
        #[cfg(target_os = "macos")]
        if let Some(current) = &self.menu
            && layout == self.layout
        {
            let t1 = std::time::Instant::now();
            menu_views::apply(current.ns_menu(), &built.cards, &built.rows, &built.hidden);
            if std::env::var_os("LCM_TIMING").is_some() {
                eprintln!(
                    "  in-place apply: {:.1} ms",
                    t1.elapsed().as_secs_f64() * 1000.0
                );
            }
            self.org_ids = built.org_ids;
            let view = active_view(&self.snapshot, &settings);
            let _ = tray.set_tooltip(Some(summary(&view)));
            tray.set_title(menu_title(&view, &settings));
            set_status_icon(tray, view.light());
            return;
        }
        let t1 = std::time::Instant::now();
        built.attach_cards();
        if std::env::var_os("LCM_TIMING").is_some() {
            eprintln!(
                "  attach views: {:.1} ms",
                t1.elapsed().as_secs_f64() * 1000.0
            );
        }
        self.row_paths = built.rows.iter().map(|(p, _)| p[0]).collect();
        self.layout = layout;
        let Built {
            menu,
            actions,
            org_ids,
            ..
        } = built;
        self.org_ids = org_ids;
        self.actions = actions;
        self.menu = Some(menu.clone());
        tray.set_menu(Some(Box::new(menu)));
        let view = active_view(&self.snapshot, &settings);
        let _ = tray.set_tooltip(Some(summary(&view)));
        tray.set_title(menu_title(&view, &settings));
        // After the title: the dot is placed relative to where the image is drawn.
        set_status_icon(tray, view.light());
    }

    fn set_org(&mut self, i: usize) {
        let Some(id) = self.org_ids.get(i).cloned() else {
            return;
        };
        {
            let mut s = self.settings.write().unwrap();
            if s.active_account == id {
                return;
            }
            s.active_account = id;
            let _ = settings::save(&s);
        }
        // Purely local: no network, the menu updates in place.
        self.rerender();
    }

    fn set_period(&mut self, p: Period) {
        {
            let mut s = self.settings.write().unwrap();
            if s.metrics_period == p {
                return;
            }
            s.metrics_period = p;
            let _ = settings::save(&s);
        }
        if self.demo {
            self.snapshot = demo_snapshot(p);
            self.snapshot.hidden = self.settings().hidden_in(&self.snapshot.apps);
        } else {
            let _ = self.poller.send(Command::SettingsChanged);
        }
        self.rerender();
    }

    fn handle(&mut self, action: Action, target: &EventLoopWindowTarget<UserEvent>) {
        match action {
            Action::Refresh => {
                let _ = self.poller.send(Command::Refresh { topology: true });
            }
            Action::OpenUrl(u) => system::open_url(&u),
            Action::Copy(t) => menu_views::copy_text(&t),
            Action::Env { env_id, action } => self.env_action(env_id, action),
            Action::Window(key) => self.open(key, target),
            Action::Update => self.install_update(),
            Action::About => {
                if menu_views::ask(
                    &format!("LC Menubar {}", env!("CARGO_PKG_VERSION")),
                    ABOUT_TEXT,
                    "Open GitHub",
                    "Close",
                ) {
                    system::open_url(AUTHOR_URL);
                }
            }
            Action::Quit => {}
        }
    }

    fn env_label(&self, env_id: &str) -> String {
        self.snapshot
            .all_environments()
            .find(|(_, e)| e.id == env_id)
            .map(|(a, e)| format!("{} · {}", a.name, e.name))
            .unwrap_or_else(|| "environment".into())
    }

    fn client_for(&self, env_id: &str) -> Result<api::Client, String> {
        let account = self
            .snapshot
            .all_environments()
            .find(|(_, e)| e.id == env_id)
            .map(|(a, _)| a.account.clone())
            .ok_or("Unknown environment, refresh and try again")?;
        let token = self
            .tokens
            .read()
            .unwrap()
            .get(&account)
            .cloned()
            .ok_or("No token for this organization")?;
        api::Client::new(token).map_err(|e| e.to_string())
    }

    /// Confirm with a native dialog, then run the API call as a background job.
    /// Checks GitHub for a newer release now and every six hours while enabled.
    fn schedule_update_checks(&self) {
        let proxy = self.proxy.clone();
        let settings = self.settings.clone();
        self.jobs.spawn(async move {
            // Let startup settle before the first network call.
            tokio::time::sleep(std::time::Duration::from_secs(20)).await;
            loop {
                if settings.read().unwrap().check_updates {
                    let result = updater::check().await.map_err(|e| format!("{e:#}"));
                    let _ = proxy.send_event(UserEvent::UpdateChecked {
                        result,
                        manual: false,
                    });
                }
                tokio::time::sleep(std::time::Duration::from_secs(6 * 3600)).await;
            }
        });
    }

    fn check_updates_now(&self) {
        let proxy = self.proxy.clone();
        self.jobs.spawn(async move {
            let result = updater::check().await.map_err(|e| format!("{e:#}"));
            let _ = proxy.send_event(UserEvent::UpdateChecked {
                result,
                manual: true,
            });
        });
    }

    fn on_update_checked(
        &mut self,
        result: Result<Option<updater::Release>, String>,
        manual: bool,
    ) {
        match result {
            Ok(found) => {
                if manual && found.is_none() {
                    self.settings_toast(
                        &format!("LC Menubar {} is up to date", updater::CURRENT),
                        false,
                    );
                }
                if found != self.update {
                    self.update = found.clone();
                    self.rerender();
                }
                // Checked from Settings: offer the install right away.
                if manual && let Some(r) = found {
                    self.settings_toast(&format!("LC Menubar {} is available", r.version), false);
                    self.install_update();
                }
            }
            Err(e) => {
                if manual {
                    self.settings_toast(&format!("Update check failed: {e}"), true);
                } else {
                    eprintln!("update check failed: {e}");
                }
            }
        }
    }

    /// Offer the update in its own window (notes, Later, Install and restart).
    fn install_update(&mut self) {
        if self.update.is_some() {
            let _ = self.proxy.send_event(UserEvent::OpenWindow(WinKey::Update));
        }
    }

    fn on_update_window(&mut self, kind: &str, target: &EventLoopWindowTarget<UserEvent>) {
        let Some(release) = self.update.clone() else {
            return self.close_key(&WinKey::Update, target);
        };
        match kind {
            "page" => system::open_url(&release.page),
            "install" => {
                self.close_key(&WinKey::Update, target);
                if !updater::can_install() || release.zip_url.is_none() {
                    // Not in a writable bundle (development build, or no
                    // permission): the release page has the download.
                    system::open_url(&release.page);
                } else {
                    self.start_install(release);
                }
            }
            _ => {}
        }
    }

    fn start_install(&mut self, release: updater::Release) {
        self.installing = true;
        self.rerender();
        let proxy = self.proxy.clone();
        self.jobs.spawn(async move {
            let event = match updater::install(&release).await {
                Ok(bundle) => UserEvent::UpdateInstalled(bundle),
                Err(e) => UserEvent::UpdateFailed(format!("{e:#}")),
            };
            let _ = proxy.send_event(event);
        });
    }

    fn env_action(&self, env_id: String, action: EnvAction) {
        let label = self.env_label(&env_id);
        // Switching asks for the branch first (when not picked from the menu).
        let action = match action {
            EnvAction::SwitchBranch(None) => {
                let Some(branch) = menu_views::prompt(
                    "Switch branch",
                    &format!("Which branch should {label} deploy?"),
                    "branch name",
                    "Continue",
                ) else {
                    return;
                };
                EnvAction::SwitchBranch(Some(branch))
            }
            other => other,
        };
        let from = self
            .snapshot
            .all_environments()
            .find(|(_, e)| e.id == env_id)
            .and_then(|(_, e)| e.latest.as_ref())
            .map(|d| d.branch.clone())
            .unwrap_or_default();
        let (title, question, button, done) = match &action {
            EnvAction::SwitchBranch(branch) => {
                let branch = branch.clone().unwrap_or_default();
                (
                    "Switch branch",
                    if from.is_empty() {
                        format!("Switch {label} to {branch} and deploy it?")
                    } else {
                        format!("Switch {label} from {from} to {branch} and deploy it?")
                    },
                    "Switch and deploy",
                    "Deployment started",
                )
            }
            EnvAction::Deploy => {
                let current = self
                    .snapshot
                    .all_environments()
                    .find(|(_, e)| e.id == env_id)
                    .and_then(|(_, e)| e.latest.as_ref())
                    .map(|d| {
                        format!(
                            "\n\nCurrently deployed: {} {}",
                            d.short_hash(),
                            first_line(&d.commit_message)
                        )
                    })
                    .unwrap_or_default();
                (
                    "Deploy",
                    format!("Start a new deployment of {label}?{current}"),
                    "Deploy",
                    "Deployment started",
                )
            }
            EnvAction::Start => (
                "Start environment",
                format!("Start {label}? This also triggers a deployment."),
                "Start",
                "Environment starting",
            ),
            EnvAction::Stop => (
                "Stop environment",
                format!(
                    "Stop {label}? It stops serving traffic and cancels any running deployment."
                ),
                "Stop",
                "Environment stopping",
            ),
            EnvAction::Purge => (
                "Purge edge cache",
                format!("Purge everything cached at the edge for {label}?"),
                "Purge",
                "Edge cache purged",
            ),
        };
        if !menu_views::confirm(title, &question, button) {
            return;
        }
        let client = match self.client_for(&env_id) {
            Ok(c) => c,
            Err(e) => return menu_views::alert("LC Menubar", &e),
        };
        let poller = self.poller.clone();
        self.jobs.spawn(async move {
            let result = match action {
                EnvAction::Deploy => client.deploy(&env_id).await,
                EnvAction::SwitchBranch(branch) => {
                    let branch = branch.unwrap_or_default();
                    match client.set_branch(&env_id, &branch).await {
                        Ok(()) => client
                            .deploy(&env_id)
                            .await
                            .map(|m| format!("Switched to {branch}. {m}")),
                        Err(e) => Err(e),
                    }
                }
                EnvAction::Start => client
                    .start_environment(&env_id)
                    .await
                    .map(|_| done.to_string()),
                EnvAction::Stop => client
                    .stop_environment(&env_id)
                    .await
                    .map(|_| done.to_string()),
                EnvAction::Purge => client
                    .purge_edge_cache(&env_id)
                    .await
                    .map(|_| done.to_string()),
            };
            let (t, b) = match result {
                Ok(msg) => (label.clone(), msg),
                Err(e) => (format!("{title} failed: {label}"), format!("{e:#}")),
            };
            tokio::task::spawn_blocking(move || system::notify(&t, &b));
            let _ = poller.send(Command::Refresh { topology: true });
        });
    }

    // ------------------------------------------------------------ windows

    fn update_activation(&self, target: &EventLoopWindowTarget<UserEvent>) {
        #[cfg(target_os = "macos")]
        {
            use tao::platform::macos::{ActivationPolicy, EventLoopWindowTargetExtMacOS};
            let any = !self.windows.is_empty();
            target.set_activation_policy_at_runtime(if any {
                ActivationPolicy::Regular
            } else {
                ActivationPolicy::Accessory
            });
            if any {
                target.show_application();
            }
        }
    }

    fn close_window(&mut self, id: WindowId, target: &EventLoopWindowTarget<UserEvent>) {
        self.windows.retain(|_, w| w.id() != id);
        self.update_activation(target);
    }

    fn close_key(&mut self, key: &WinKey, target: &EventLoopWindowTarget<UserEvent>) {
        if let Some(id) = self.windows.get(key).map(WebWindow::id) {
            self.close_window(id, target);
        }
    }

    fn open(&mut self, key: WinKey, target: &EventLoopWindowTarget<UserEvent>) {
        if let Some(w) = self.windows.get(&key) {
            w.show();
            return;
        }
        let (title, size, html) = match &key {
            WinKey::Settings => {
                let init = self.settings_init();
                (
                    "LC Menubar Settings".to_string(),
                    (820.0, 600.0),
                    windows::settings_html(&init),
                )
            }
            WinKey::Vars(env) => {
                let label = self.env_label(env);
                (
                    format!("{label} — Environment variables"),
                    (760.0, 620.0),
                    windows::vars_html(&json!({ "title": label })),
                )
            }
            WinKey::Logs(env) => {
                let label = self.env_label(env);
                (
                    format!("{label} — Logs"),
                    (980.0, 680.0),
                    windows::logs_html(&json!({ "title": label })),
                )
            }
            WinKey::Update => {
                let Some(release) = self.update.clone() else {
                    return;
                };
                let icon = charts::app_icon_png(128)
                    .map(|png| format!("data:image/png;base64,{}", windows::base64(&png)))
                    .unwrap_or_default();
                (
                    "Software Update".to_string(),
                    (560.0, 520.0),
                    windows::update_html(&json!({
                        "version": release.version,
                        "current": updater::CURRENT,
                        "groups": changelog::note_groups(&release.notes),
                        "canInstall": updater::can_install() && release.zip_url.is_some(),
                        "icon": icon,
                    })),
                )
            }
            WinKey::Commands(env) => {
                let label = self.env_label(env);
                let favorites = self.settings().favorite_commands;
                (
                    format!("{label} — Commands"),
                    (860.0, 640.0),
                    windows::commands_html(&json!({ "title": label, "favorites": favorites })),
                )
            }
        };
        let proxy = self.proxy.clone();
        let ipc_key = key.clone();
        let t_open = std::time::Instant::now();
        self.window_opened_at = Some(t_open);
        match WebWindow::open(target, &title, size, html, move |msg| {
            let _ = proxy.send_event(UserEvent::Ipc {
                key: ipc_key.clone(),
                msg,
            });
        }) {
            Ok(w) => {
                if std::env::var_os("LCM_TIMING").is_some() {
                    eprintln!(
                        "window {key:?}: created in {:.1} ms",
                        t_open.elapsed().as_secs_f64() * 1000.0
                    );
                }
                self.windows.insert(key.clone(), w);
                // Fallback in case the page never reports `ready`.
                let proxy = self.proxy.clone();
                let show_key = key.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                    let _ = proxy.send_event(UserEvent::ShowWindow(show_key));
                });
                match &key {
                    WinKey::Vars(env) => self.load_vars(env),
                    WinKey::Commands(env) => self.load_history(env),
                    _ => {}
                }
            }
            Err(e) => menu_views::alert("Could not open window", &e.to_string()),
        }
        self.update_activation(target);
    }

    fn to_window(&self, key: WinKey, func: &'static str, arg: Value) {
        let _ = self
            .proxy
            .send_event(UserEvent::ToWindow { key, func, arg });
    }

    fn settings_init(&self) -> Value {
        let s = self.settings();
        let tokens = self.tokens.read().unwrap();
        let envs: Vec<Value> = self
            .snapshot
            .all_environments()
            .map(|(a, e)| json!({ "id": e.id, "appId": a.id, "app": a.name, "org": a.org, "name": e.name }))
            .collect();
        json!({
            "settings": s,
            "accounts": s.accounts.iter().map(|a| json!({
                "id": a.id, "name": a.name, "connected": tokens.contains_key(&a.id)
            })).collect::<Vec<_>>(),
            "envAccount": tokens.contains_key(ENV_ACCOUNT),
            "envs": envs,
            "version": env!("CARGO_PKG_VERSION"),
            "changelog": changelog::releases(),
            // Development: open Settings on a tab (e.g. LCM_TAB=news), for screenshots.
            "tab": std::env::var("LCM_TAB").ok(),
        })
    }

    fn refresh_settings_accounts(&self, toast: Option<&str>) {
        if let Some(w) = self.windows.get(&WinKey::Settings) {
            w.call("setInit", &self.settings_init());
            if let Some(t) = toast {
                w.toast(t, false);
            }
        }
    }

    fn on_ipc(&mut self, key: WinKey, msg: &str, target: &EventLoopWindowTarget<UserEvent>) {
        let msg: Value = match serde_json::from_str(msg) {
            Ok(v) => v,
            Err(e) => return eprintln!("bad ipc message: {e}"),
        };
        let kind = msg
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if kind == "close" {
            return self.close_key(&key, target);
        }
        if kind == "ready" {
            if let Some(w) = self.windows.get(&key) {
                w.show();
            }
            if let (Some(t), true) = (
                self.window_opened_at.take(),
                std::env::var_os("LCM_TIMING").is_some(),
            ) {
                eprintln!(
                    "window {key:?}: page ready after {:.1} ms",
                    t.elapsed().as_secs_f64() * 1000.0
                );
            }
            return;
        }
        match key.clone() {
            WinKey::Settings => self.on_settings(&kind, msg),
            WinKey::Vars(env) => self.on_vars(&env, &kind, msg),
            WinKey::Logs(env) => self.on_logs(&env, &kind, msg),
            WinKey::Commands(env) => self.on_commands(&env, &kind, msg),
            WinKey::Update => self.on_update_window(&kind, target),
        }
    }

    fn settings_toast(&self, text: &str, error: bool) {
        if let Some(w) = self.windows.get(&WinKey::Settings) {
            w.toast(text, error);
        }
    }

    fn on_settings(&mut self, kind: &str, msg: Value) {
        match kind {
            "open_author" => system::open_url(AUTHOR_URL),
            "check_updates" => {
                if self.demo {
                    return self.settings_toast("Update checks are off in demo mode", false);
                }
                self.settings_toast("Checking for updates…", false);
                self.check_updates_now();
            }
            "save" => {
                let Ok(mut new) = serde_json::from_value::<Settings>(msg["settings"].clone())
                else {
                    return self.settings_toast("Could not read settings", true);
                };
                let old = self.settings();
                // Accounts are managed separately; never trust the page's copy.
                new.accounts = old.accounts.clone();
                let new = new.sanitized();
                if new.start_at_login != old.start_at_login
                    && let Err(e) = settings::apply_login_item(new.start_at_login)
                {
                    self.settings_toast(&format!("Start at login: {e}"), true);
                    return self.refresh_settings_accounts(None);
                }
                *self.settings.write().unwrap() = new.clone();
                match settings::save(&new) {
                    Ok(()) => self.settings_toast("Saved", false),
                    Err(e) => self.settings_toast(&format!("Could not save settings: {e}"), true),
                }
                // Apply hidden projects and environments at once, before the next poll.
                self.snapshot.hidden = new.hidden_in(&self.snapshot.apps);
                let _ = self.poller.send(Command::SettingsChanged);
                self.rerender();
            }
            "addAccount" => {
                let token = msg["token"].as_str().unwrap_or_default().trim().to_string();
                if token.is_empty() {
                    return self.settings_toast("Paste a token first", true);
                }
                let proxy = self.proxy.clone();
                self.settings_toast("Checking token…", false);
                self.jobs.spawn(async move {
                    let result = match api::Client::new(token.clone()) {
                        Ok(c) => c.organization().await,
                        Err(e) => Err(e),
                    };
                    match result {
                        Ok(name) => {
                            let id = new_account_id();
                            if let Err(e) = system::save_token(&id, &token) {
                                let _ = proxy.send_event(UserEvent::ToWindow {
                                    key: WinKey::Settings,
                                    func: "toast",
                                    arg: json!({ "text": format!("Keychain: {e}"), "error": true }),
                                });
                                return;
                            }
                            let _ = proxy.send_event(UserEvent::AccountAdded { id, name, token });
                        }
                        Err(e) => {
                            let _ = proxy.send_event(UserEvent::ToWindow {
                                key: WinKey::Settings,
                                func: "toast",
                                arg: json!({ "text": format!("Token rejected: {e:#}"), "error": true }),
                            });
                        }
                    }
                });
            }
            "removeAccount" => {
                let id = msg["id"].as_str().unwrap_or_default().to_string();
                let name = self
                    .settings()
                    .accounts
                    .iter()
                    .find(|a| a.id == id)
                    .map(|a| a.name.clone())
                    .unwrap_or_else(|| "this organization".into());
                if !menu_views::confirm(
                    &format!("Remove {name}?"),
                    "Its API token is deleted from the Keychain and its projects leave the menu. You can connect it again with a new token.",
                    "Remove",
                ) {
                    return;
                }
                if let Err(e) = system::delete_token(&id) {
                    return self.settings_toast(&format!("Keychain: {e}"), true);
                }
                self.tokens.write().unwrap().remove(&id);
                {
                    let mut s = self.settings.write().unwrap();
                    s.accounts.retain(|a| a.id != id);
                    let _ = settings::save(&s);
                }
                self.push_credentials();
                self.snapshot.apps.retain(|a| a.account != id);
                self.rerender();
                self.refresh_settings_accounts(Some("Organization removed"));
            }
            _ => {}
        }
    }

    // ---------------------------------------------------- environment variables

    fn load_vars(&self, env_id: &str) {
        let key = WinKey::Vars(env_id.to_string());
        let (proxy, env_id, client) = (
            self.proxy.clone(),
            env_id.to_string(),
            self.client_for(env_id),
        );
        self.jobs.spawn(async move {
            let (func, arg) = match client {
                Ok(c) => match c.variables(&env_id).await {
                    Ok(vars) => ("loaded", json!({ "vars": vars })),
                    Err(e) => ("failed", json!({ "text": format!("{e:#}") })),
                },
                Err(e) => ("failed", json!({ "text": e })),
            };
            let _ = proxy.send_event(UserEvent::ToWindow { key, func, arg });
        });
    }

    fn on_vars(&mut self, env_id: &str, kind: &str, msg: Value) {
        let key = WinKey::Vars(env_id.to_string());
        match kind {
            "reload" => self.load_vars(env_id),
            "save" => {
                // A payload that does not parse must never turn into "delete everything".
                let parse = |v: &Value| serde_json::from_value::<Vec<(String, String)>>(v.clone());
                let (Ok(original), Ok(edited)) = (parse(&msg["original"]), parse(&msg["edited"]))
                else {
                    return self.to_window(
                        key,
                        "failed",
                        json!({ "text": "Could not read the edited variables" }),
                    );
                };
                let changes = match vars::diff(&original, &edited) {
                    Ok(c) => c,
                    Err(e) => return self.to_window(key, "failed", json!({ "text": e })),
                };
                if changes.is_empty() {
                    return self.to_window(key, "saved", json!({ "text": "No changes" }));
                }
                let mut detail = vec![];
                if !changes.upserts.is_empty() {
                    let keys: Vec<&str> = changes.upserts.iter().map(|(k, _)| k.as_str()).collect();
                    detail.push(format!("Set: {}", keys.join(", ")));
                }
                if !changes.deletes.is_empty() {
                    detail.push(format!("Delete: {}", changes.deletes.join(", ")));
                }
                if !menu_views::confirm(
                    &format!("Save {} to {}?", changes.summary(), self.env_label(env_id)),
                    &model::truncate(&detail.join("\n\n"), 900),
                    "Save",
                ) {
                    return self.to_window(key, "failed", json!({ "text": "Not saved" }));
                }
                let (proxy, env, client) = (
                    self.proxy.clone(),
                    env_id.to_string(),
                    self.client_for(env_id),
                );
                let reload_key = key.clone();
                self.jobs.spawn(async move {
                    let result: Result<String, String> = async {
                        let c = client?;
                        c.set_variables(&env, &changes.upserts)
                            .await
                            .map_err(|e| format!("{e:#}"))?;
                        c.delete_variables(&env, &changes.deletes)
                            .await
                            .map_err(|e| format!("{e:#}"))?;
                        Ok(format!("{}. Deploy to apply", changes.summary()))
                    }
                    .await;
                    let (func, arg) = match &result {
                        Ok(t) => ("saved", json!({ "text": t })),
                        Err(e) => ("failed", json!({ "text": e })),
                    };
                    let _ = proxy.send_event(UserEvent::ToWindow { key, func, arg });
                    if result.is_ok() {
                        let _ = proxy.send_event(UserEvent::Ipc {
                            key: reload_key,
                            msg: r#"{"type":"reload"}"#.into(),
                        });
                    }
                });
            }
            "deploy" => self.env_action(env_id.to_string(), EnvAction::Deploy),
            _ => {}
        }
    }

    // ------------------------------------------------------------------- logs

    fn on_logs(&mut self, env_id: &str, kind: &str, msg: Value) {
        if kind != "fetch" {
            return;
        }
        let key = WinKey::Logs(env_id.to_string());
        let mode = msg["mode"].as_str().unwrap_or("replace").to_string();
        let query: api::LogQuery = match serde_json::from_value(msg["query"].clone()) {
            Ok(q) => q,
            Err(e) => return self.to_window(key, "logsFailed", json!({ "text": e.to_string() })),
        };
        let (proxy, env, client) = (
            self.proxy.clone(),
            env_id.to_string(),
            self.client_for(env_id),
        );
        self.jobs.spawn(async move {
            let (func, arg) = match client {
                Ok(c) => match c.logs(&env, &query).await {
                    Ok(v) => (
                        "logsLoaded",
                        json!({ "mode": mode, "data": v["data"], "meta": v["meta"] }),
                    ),
                    Err(e) => ("logsFailed", json!({ "text": format!("{e:#}") })),
                },
                Err(e) => ("logsFailed", json!({ "text": e })),
            };
            let _ = proxy.send_event(UserEvent::ToWindow { key, func, arg });
        });
    }

    // --------------------------------------------------------------- commands

    fn load_history(&self, env_id: &str) {
        let key = WinKey::Commands(env_id.to_string());
        let (proxy, env, client) = (
            self.proxy.clone(),
            env_id.to_string(),
            self.client_for(env_id),
        );
        self.jobs.spawn(async move {
            let arg = match client {
                Ok(c) => match c.commands(&env).await {
                    Ok(v) => json!({ "items": v["data"] }),
                    Err(e) => json!({ "error": format!("{e:#}") }),
                },
                Err(e) => json!({ "error": e }),
            };
            let _ = proxy.send_event(UserEvent::ToWindow {
                key,
                func: "historyLoaded",
                arg,
            });
        });
    }

    fn on_commands(&mut self, env_id: &str, kind: &str, msg: Value) {
        let key = WinKey::Commands(env_id.to_string());
        match kind {
            "history" => self.load_history(env_id),
            "favorites" => {
                let favs: Vec<String> =
                    serde_json::from_value(msg["favorites"].clone()).unwrap_or_default();
                let mut s = self.settings.write().unwrap();
                s.favorite_commands = favs;
                let _ = settings::save(&s);
            }
            "run" => {
                let command = msg["command"]
                    .as_str()
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if command.is_empty() {
                    return;
                }
                let label = self.env_label(env_id);
                let client = match self.client_for(env_id) {
                    Ok(c) => c,
                    Err(e) => return self.to_window(key, "commandFailed", json!({ "text": e })),
                };
                let question = format!("Run this command on {label}?\n\n{command}");
                if !menu_views::confirm("Run command", &question, "Run") {
                    return self.to_window(key, "commandCancelled", Value::Null);
                }
                self.jobs.spawn(run_command(
                    client,
                    env_id.to_string(),
                    command,
                    key,
                    self.proxy.clone(),
                ));
            }
            _ => {}
        }
    }
}

/// Start a command, then poll it until it finishes, streaming updates to the window.
async fn run_command(
    client: api::Client,
    env: String,
    command: String,
    key: WinKey,
    proxy: EventLoopProxy<UserEvent>,
) {
    let send = |func: &'static str, arg: Value| {
        let _ = proxy.send_event(UserEvent::ToWindow {
            key: key.clone(),
            func,
            arg,
        });
    };
    let created = match client.run_command(&env, &command).await {
        Ok(v) => v,
        Err(e) => return send("commandFailed", json!({ "text": format!("{e:#}") })),
    };
    let Some(id) = created
        .pointer("/data/id")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return send(
            "commandFailed",
            json!({ "text": "The API did not return a command id" }),
        );
    };
    send("commandUpdate", created["data"].clone());
    let started = std::time::Instant::now();
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        match client.command(&id).await {
            Ok(v) => {
                let status = v
                    .pointer("/data/attributes/status")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                send("commandUpdate", v["data"].clone());
                if matches!(status, "command.success" | "command.failure") {
                    break;
                }
            }
            Err(e) => send("commandWarning", json!({ "text": format!("{e:#}") })),
        }
        if started.elapsed() > std::time::Duration::from_secs(30 * 60) {
            send(
                "commandWarning",
                json!({ "text": "Stopped watching after 30 minutes" }),
            );
            break;
        }
    }
    send("commandDone", Value::Null);
}

/// Hidden main menu so Cmd-C/V/X/A/Z/W work in windows (accessory apps have no
/// visible menu bar, but key equivalents still route through it).
fn install_edit_menu() {
    let menu = Menu::new();
    let app_menu = Submenu::new("LC Menubar", true);
    let _ = app_menu.append(&PredefinedMenuItem::close_window(None));
    let _ = app_menu.append(&PredefinedMenuItem::quit(None));
    let edit = Submenu::new("Edit", true);
    for item in [
        PredefinedMenuItem::undo(None),
        PredefinedMenuItem::redo(None),
        PredefinedMenuItem::separator(),
        PredefinedMenuItem::cut(None),
        PredefinedMenuItem::copy(None),
        PredefinedMenuItem::paste(None),
        PredefinedMenuItem::select_all(None),
    ] {
        let _ = edit.append(&item);
    }
    let _ = menu.append(&app_menu);
    let _ = menu.append(&edit);
    #[cfg(target_os = "macos")]
    menu.init_for_nsapp();
    std::mem::forget(menu); // must live for the whole program
}

/// Organizations in menu order: (account id, name).
fn orgs(s: &Snapshot) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vec![];
    for a in &s.apps {
        if !out.iter().any(|(id, _)| id == &a.account) {
            out.push((a.account.clone(), a.org.clone()));
        }
    }
    out
}

/// The selected organization, if it still exists and there is a choice.
fn active_account(s: &Snapshot, settings: &Settings) -> Option<String> {
    let orgs = orgs(s);
    settings
        .active_account
        .clone()
        .filter(|a| orgs.len() > 1 && orgs.iter().any(|(id, _)| id == a))
}

/// The snapshot as the organization switch shows it.
fn active_view(s: &Snapshot, settings: &Settings) -> Snapshot {
    let Some(active) = active_account(s, settings) else {
        return s.clone();
    };
    let mut v = s.clone();
    v.apps.retain(|a| a.account == active);
    v.usage.retain(|(id, _, _)| *id == active);
    v
}

fn summary(s: &Snapshot) -> String {
    let (mut building, mut failed, mut total) = (0, 0, 0);
    for (_, e) in s.environments() {
        if let Some(d) = &e.latest {
            total += 1;
            match d.phase() {
                Phase::Building => building += 1,
                Phase::Failed => failed += 1,
                _ => {}
            }
        }
    }
    if total == 0 {
        return match &s.error {
            Some(e) => truncate(e, 60),
            None => "No deployments yet".into(),
        };
    }
    let mut parts = vec![];
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    if building > 0 {
        parts.push(format!("{building} deploying"));
    }
    if parts.is_empty() {
        parts.push(format!("All {total} deployed"));
    }
    parts.join(", ")
}

fn total_spend(s: &Snapshot) -> Option<String> {
    let first = s.usage.first()?;
    let currency = &first.2.currency;
    let cents: i64 = s
        .usage
        .iter()
        .filter(|u| &u.2.currency == currency)
        .map(|u| u.2.spend_cents)
        .sum();
    Some(api::fmt_money(cents, currency))
}

/// CPU for the compact row column: whole percent, "<1%" for tiny non-zero values.
fn fmt_pct_short(v: f64) -> String {
    if v > 0.0 && v < 1.0 {
        "<1%".into()
    } else {
        format!("{v:.0}%")
    }
}

const PERIODS: [Period; 4] = [Period::H1, Period::H6, Period::H24, Period::D7];

fn period_label(p: Period) -> &'static str {
    match p {
        Period::H1 => "last hour",
        Period::H6 => "last 6 hours",
        Period::H24 => "last 24 hours",
        Period::D7 => "last 7 days",
    }
}

// ---------------------------------------------------------------- menu building

struct Built {
    menu: Menu,
    actions: HashMap<MenuId, Action>,
    cards: Vec<(menu_views::Path, Card)>,
    rows: Vec<(menu_views::Path, menu_views::Row)>,
    signature: Vec<String>,
    /// Top-level items and whether the organization filter hides them.
    hidden: Vec<(menu_views::Path, bool)>,
    org_ids: Vec<Option<String>>,
}

impl Built {
    /// Install card views and row styles on the native items that are shown.
    /// Must run after every submenu has been attached to the menu.
    fn attach_cards(&self) {
        // A new menu gets new views: views still held by the previous menu's
        // items come up blank when moved over.
        #[cfg(target_os = "macos")]
        menu_views::forget_views();
        #[cfg(target_os = "macos")]
        menu_views::apply(self.menu.ns_menu(), &self.cards, &self.rows, &self.hidden);
    }
}

struct B {
    /// Top-level item index → account it belongs to (for the organization filter).
    owned: Vec<(usize, String)>,
    /// Everything that would make an in-place update wrong: action labels,
    /// enabled state and environment ids, in menu order.
    signature: Vec<String>,
    actions: HashMap<MenuId, Action>,
    cards: Vec<(menu_views::Path, Card)>,
    rows: Vec<(menu_views::Path, menu_views::Row)>,
}

impl B {
    fn item(&mut self, text: &str, action: Option<Action>, accel: Option<Accelerator>) -> MenuItem {
        // The action's payload is part of the layout: an in-place update keeps the
        // current items and their actions, so a changed payload needs a rebuild.
        self.signature.push(format!("{text}:{action:?}"));
        let it = MenuItem::new(text, action.is_some(), accel);
        if let Some(a) = action {
            self.actions.insert(it.id().clone(), a);
        }
        it
    }

    /// Placeholder in a submenu; returns its index for the path once the submenu
    /// is placed.
    fn placeholder_in_sub(sub: &Submenu) -> usize {
        let index = sub.items().len();
        let _ = sub.append(&MenuItem::new(" ", false, None));
        index
    }

    fn card_in_menu(&mut self, menu: &Menu, card: Card) {
        let index = menu.items().len();
        let _ = menu.append(&MenuItem::new(" ", false, None));
        self.cards.push((vec![index], card));
    }
}

/// Rendered PNGs by input hash, plus insertion order for eviction.
type ChartMemo = (HashMap<u64, Vec<u8>>, std::collections::VecDeque<u64>);

thread_local! {
    /// Rendered chart images by input (insertion order kept for eviction), so
    /// unchanged charts, and ranges switched back to, are not redrawn.
    static CHART_MEMO: std::cell::RefCell<ChartMemo> =
        std::cell::RefCell::new(Default::default());
}

const CHART_MEMO_CAP: usize = 256;

/// Memoize a chart render by a hash of its inputs.
fn chart_memo(
    key: impl std::hash::Hash,
    render: impl FnOnce() -> Option<Vec<u8>>,
) -> Option<Vec<u8>> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut h);
    charts::is_dark_palette().hash(&mut h);
    let k = h.finish();
    if let Some(v) = CHART_MEMO.with(|m| m.borrow().0.get(&k).cloned()) {
        return Some(v);
    }
    let v = render()?;
    CHART_MEMO.with(|m| {
        let mut m = m.borrow_mut();
        let (map, order) = &mut *m;
        map.insert(k, v.clone());
        order.push_back(k);
        while order.len() > CHART_MEMO_CAP {
            if let Some(old) = order.pop_front() {
                map.remove(&old);
            }
        }
    });
    Some(v)
}

fn bits(v: &[f64]) -> Vec<u64> {
    v.iter().map(|x| x.to_bits()).collect()
}

fn chart_w() -> f32 {
    (menu_views::CARD_W - 2.0 * menu_views::INSET) as f32
}

fn metric_cards(m: &Metrics, selected: Period) -> Vec<Card> {
    let mut cards = vec![];
    // Data from another range is shown dimmed until the selected one arrives.
    let dim = m.period != selected;
    let loading = format!("Loading {}…", period_label(selected));
    let range = period_label(m.period);
    let pct_card =
        |title: &str, hue: charts::Hue, metric: &Option<model::Metric>| -> Option<Card> {
            let metric = metric.as_ref()?;
            let series: Vec<Vec<f64>> = metric.series.iter().map(|s| s.points.clone()).collect();
            let now = metric
                .series
                .iter()
                .filter_map(|s| s.last())
                .reduce(f64::max);
            let peak = metric
                .series
                .iter()
                .filter_map(|s| s.peak())
                .reduce(f64::max);
            let avg = metric
                .series
                .iter()
                .filter_map(|s| s.average)
                .reduce(f64::max);
            // A fixed 0–100 % scale, as in Laravel Cloud's dashboard: 30 % looks like 30 %.
            let key: Vec<Vec<u64>> = series.iter().map(|s| bits(s)).collect();
            let png = chart_memo(("line100", key, hue as u8), || {
                charts::line_chart(&series, hue, 100.0, chart_w(), 40.0)
            })?;
            let mut caption = vec![];
            if let Some(a) = avg {
                caption.push(format!("avg {}", fmt_pct(Some(a))));
            }
            caption.push(format!("peak {}", fmt_pct(peak)));
            if metric.series.len() > 1 {
                caption.push(format!("{} instances", metric.series.len()));
            }
            caption.push(range.to_string());
            Some(Card::Chart {
                title: title.into(),
                value: fmt_pct(now),
                caption: if dim {
                    loading.clone()
                } else {
                    caption.join(" · ")
                },
                png,
                chart_h: 40.0,
                dim,
            })
        };
    cards.extend(pct_card("CPU", charts::Hue::Blue, &m.cpu));
    cards.extend(pct_card("Memory", charts::Hue::Teal, &m.memory));

    if let Some(req) = &m.requests {
        let len = req.series.iter().map(|s| s.points.len()).max().unwrap_or(0);
        let mut total = vec![0.0; len];
        let mut errors = vec![0.0; len];
        for s in &req.series {
            let offset = len - s.points.len();
            let is_error = s.label.trim_start().starts_with('5');
            for (i, v) in s.points.iter().enumerate() {
                total[offset + i] += v;
                if is_error {
                    errors[offset + i] += v;
                }
            }
        }
        let sum = req
            .total
            .unwrap_or_else(|| req.series.iter().filter_map(|s| s.average).sum());
        let err_sum: f64 = req
            .series
            .iter()
            .filter(|s| s.label.trim_start().starts_with('5'))
            .filter_map(|s| s.average)
            .sum();
        let caption = if sum > 0.0 {
            format!(
                "5xx {} ({:.1}%) · {range}",
                fmt_count(err_sum),
                err_sum / sum * 100.0
            )
        } else {
            format!("No requests · {range}")
        };
        if let Some(png) = chart_memo(("bars", bits(&total), bits(&errors)), || {
            charts::bar_chart(&total, &errors, chart_w(), 32.0)
        }) {
            let caption = if dim { loading.clone() } else { caption };
            cards.push(Card::Chart {
                title: "Requests".into(),
                value: fmt_count(sum),
                caption,
                png,
                chart_h: 32.0,
                dim,
            });
        }
    }

    let now = |metric: &Option<model::Metric>| {
        metric
            .as_ref()
            .map(|m| fmt_count(m.series.iter().filter_map(|s| s.last()).sum()))
    };
    let mut stats = vec![];
    if let Some(r) = now(&m.replicas) {
        stats.push(("Replicas".to_string(), r));
    }
    if let Some(w) = now(&m.workers) {
        stats.push(("Web workers".to_string(), w));
    }
    if !stats.is_empty() {
        cards.push(Card::Stats { items: stats });
    }
    cards
}

/// Credits for the About dialog and Settings.
const AUTHOR_URL: &str = "https://github.com/mstaack";
const ABOUT_TEXT: &str = "Made by Max (github.com/mstaack), 100% vibe-coded with Claude.\n\nLC Menubar is an independent project, not affiliated with Laravel Holdings Inc.";

/// Text next to the status dot: the total spend, when enabled.
fn menu_title(view: &Snapshot, settings: &Settings) -> Option<String> {
    (settings.show_spend && settings.show_spend_in_menu_bar)
        .then(|| total_spend(view))
        .flatten()
}

fn deployment_card(env: &Environment, now: chrono::DateTime<Utc>) -> Card {
    let Some(d) = &env.latest else {
        return Card::Status {
            title: "No deployments yet".into(),
            dot: Some(charts::NEUTRAL),
            lines: vec![(format!("Environment {}", env.status), Tone::Muted)],
        };
    };
    let phase = d.phase();
    let mut lines = vec![];
    let msg = first_line(&d.commit_message);
    if !msg.is_empty() {
        lines.push((truncate(msg, 48), Tone::Primary));
    }
    let mut meta = format!("{} · {}", d.short_hash(), d.branch);
    if let Some(a) = &d.commit_author {
        meta.push_str(&format!(" · {a}"));
    }
    lines.push((meta, Tone::Mono));
    let mut when = vec![];
    if let Some(t) = d.when() {
        when.push(ago(t, now));
    }
    if let (Some(s), Some(f)) = (d.started_at, d.finished_at) {
        let secs = (f - s).num_seconds().max(0);
        when.push(format!("took {}m {:02}s", secs / 60, secs % 60));
    }
    if env.status != "running" {
        when.push(format!("environment {}", env.status));
    }
    lines.push((when.join(" · "), Tone::Secondary));
    if let Some(r) = d
        .failure_reason
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
    {
        // Red only for failed deployments; Cloud also sets a reason on others.
        let tone = if phase == Phase::Failed {
            Tone::Negative
        } else {
            Tone::Secondary
        };
        for l in model::wrap(r, 52, 4) {
            lines.push((l, tone));
        }
    }
    Card::Status {
        title: phase.label().into(),
        dot: Some(phase.color()),
        lines,
    }
}

/// `update`: a newer version to offer, and whether it is being installed.
fn build_menu(
    s: &Snapshot,
    settings: &Settings,
    loading: bool,
    update: Option<(&str, bool)>,
) -> Built {
    let menu = Menu::new();
    let mut b = B {
        owned: vec![],
        signature: vec![],
        actions: HashMap::new(),
        cards: vec![],
        rows: vec![],
    };
    let now = Utc::now();

    let full = s;
    let view = active_view(full, settings);
    let s = &view;
    let active = active_account(full, settings);
    let org_list = orgs(full);
    let org_ids: Vec<Option<String>> = std::iter::once(None)
        .chain(org_list.iter().map(|(id, _)| Some(id.clone())))
        .collect();

    // Header card: overall state.
    let light = s.light();
    let dot_color = match light {
        Light::Green => charts::POSITIVE,
        Light::Yellow => charts::WARNING,
        Light::Red => charts::NEGATIVE,
        Light::Grey => charts::NEUTRAL,
    };
    let mut lines = vec![];
    if let Some(at) = s.fetched_at {
        lines.push((
            format!("Updated {}", at.with_timezone(&Local).format("%H:%M:%S")),
            Tone::Secondary,
        ));
    }
    for (org, err) in &s.org_errors {
        lines.push((truncate(&format!("{org}: {err}"), 52), Tone::Negative));
    }
    if let Some(err) = s.error.as_ref().filter(|e| {
        !s.org_errors
            .iter()
            .any(|(o, x)| e.contains(o.as_str()) && e.contains(x.as_str()))
    }) {
        lines.push((truncate(err, 52), Tone::Negative));
    }
    b.card_in_menu(
        &menu,
        Card::Status {
            title: if loading {
                "Loading".into()
            } else {
                summary(s)
            },
            dot: Some(dot_color),
            lines,
        },
    );

    // Organization switch. Every organization's rows stay in the menu and are
    // hidden or shown, so switching is instant and works while the menu is open.
    let multi_org = org_list.len() > 1;
    if multi_org {
        let selected = org_ids.iter().position(|id| *id == active).unwrap_or(0);
        let labels = std::iter::once("All".to_string())
            .chain(org_list.iter().map(|(_, n)| n.clone()))
            .collect();
        b.card_in_menu(
            &menu,
            Card::Segments {
                kind: menu_views::Segment::Org,
                title: "Organization".into(),
                labels,
                selected,
            },
        );
        b.signature.push(format!("orgs:{org_list:?}"));
    }

    let s = full;
    for app in &s.apps {
        let envs: Vec<_> = app
            .environments
            .iter()
            .filter(|e| !s.hidden.contains(&e.id))
            .collect();
        if envs.is_empty() {
            continue;
        }
        b.owned.push((menu.items().len(), app.account.clone()));
        let _ = menu.append(&PredefinedMenuItem::separator());
        let heading = if multi_org && active.is_none() {
            format!("{}  ·  {}", app.name, app.org)
        } else {
            app.name.clone()
        };
        b.owned.push((menu.items().len(), app.account.clone()));
        b.card_in_menu(&menu, Card::Heading { text: heading });

        for env in envs {
            let mut meta = vec![];
            if let Some(d) = &env.latest {
                meta.push(d.branch.clone());
                if let Some(t) = d.when() {
                    meta.push(ago(t, now));
                }
            } else {
                meta.push("no deployments".into());
            }
            let cpu = env
                .metrics
                .as_ref()
                .filter(|_| settings.metrics_enabled && settings.show_cpu_in_menu)
                .and_then(|m| m.cpu_now())
                .map(|c| format!("CPU {}", fmt_pct_short(c)));
            let replicas = env
                .metrics
                .as_ref()
                .filter(|_| settings.metrics_enabled && settings.show_replicas_in_menu)
                .and_then(|m| m.replicas.as_ref())
                .map(|r| r.series.iter().filter_map(|s| s.last()).sum::<f64>())
                .filter(|n| *n > 0.0)
                .map(|n| format!("{}×", fmt_count(n)));
            let right = [cpu, replicas]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
            let sub = Submenu::new(format!("{}   {}", env.name, meta.join(" · ")), true);
            let mut sub_cards: Vec<(usize, Card)> = vec![];
            let row = menu_views::Row {
                dot: env
                    .latest
                    .as_ref()
                    .map(|d| d.phase().color())
                    .unwrap_or(charts::NEUTRAL),
                name: env.name.clone(),
                meta: meta.join(" · "),
                right,
            };

            // Submenu: deployment card, metrics cards, range, actions.
            sub_cards.push((B::placeholder_in_sub(&sub), deployment_card(env, now)));
            if settings.metrics_enabled {
                match &env.metrics {
                    Some(m) => {
                        for card in metric_cards(m, settings.metrics_period) {
                            sub_cards.push((B::placeholder_in_sub(&sub), card));
                        }
                    }
                    None => {
                        let _ = sub.append(&MenuItem::new(
                            "Metrics loading or unavailable",
                            false,
                            None,
                        ));
                    }
                }
                let selected = PERIODS
                    .iter()
                    .position(|p| *p == settings.metrics_period)
                    .unwrap_or(0);
                let labels = PERIODS.iter().map(|p| p.as_str().to_string()).collect();
                sub_cards.push((
                    B::placeholder_in_sub(&sub),
                    Card::Segments {
                        kind: menu_views::Segment::Range,
                        title: "Range".into(),
                        labels,
                        selected,
                    },
                ));
            }
            let _ = sub.append(&PredefinedMenuItem::separator());

            let building = env
                .latest
                .as_ref()
                .is_some_and(|d| d.phase() == Phase::Building);
            let id = env.id.clone();
            let env_act = |a: EnvAction| {
                Some(Action::Env {
                    env_id: id.clone(),
                    action: a,
                })
            };
            let _ = sub.append(&b.item(
                if building {
                    "Deploying…"
                } else {
                    "Deploy now…"
                },
                if building {
                    None
                } else {
                    env_act(EnvAction::Deploy)
                },
                None,
            ));
            // Switch branch & deploy: recent branches of the project, or any other.
            let current = env
                .latest
                .as_ref()
                .map(|d| d.branch.clone())
                .unwrap_or_default();
            let branch_menu = Submenu::new("Switch branch", !building);
            let choices = app.branch_choices();
            for branch in &choices {
                let is_current = *branch == current;
                let _ = branch_menu.append(&b.item(
                    &if is_current {
                        format!("{branch} (current)")
                    } else {
                        branch.clone()
                    },
                    (!is_current).then(|| Action::Env {
                        env_id: id.clone(),
                        action: EnvAction::SwitchBranch(Some(branch.clone())),
                    }),
                    None,
                ));
            }
            if !choices.is_empty() {
                let _ = branch_menu.append(&PredefinedMenuItem::separator());
            }
            let _ = branch_menu.append(&b.item(
                "Other branch…",
                env_act(EnvAction::SwitchBranch(None)),
                None,
            ));
            let _ = sub.append(&branch_menu);
            let _ = sub.append(&b.item(
                "Logs…",
                Some(Action::Window(WinKey::Logs(id.clone()))),
                None,
            ));
            let _ = sub.append(&b.item(
                "Run command…",
                Some(Action::Window(WinKey::Commands(id.clone()))),
                None,
            ));
            let _ = sub.append(&b.item(
                "Environment variables…",
                Some(Action::Window(WinKey::Vars(id.clone()))),
                None,
            ));
            let _ = sub.append(&PredefinedMenuItem::separator());
            if let Some(domain) = &env.domain {
                let url = if domain.starts_with("http") {
                    domain.clone()
                } else {
                    format!("https://{domain}")
                };
                let _ = sub.append(&b.item("Open site", Some(Action::OpenUrl(url)), None));
            }
            if let Some(d) = env.latest.as_ref().filter(|d| !d.commit_hash.is_empty()) {
                let _ = sub.append(&b.item(
                    "Copy commit hash",
                    Some(Action::Copy(d.commit_hash.clone())),
                    None,
                ));
            }
            let _ = sub.append(&PredefinedMenuItem::separator());
            let stopped = matches!(env.status.as_str(), "stopped");
            let _ = sub.append(&b.item(
                if stopped {
                    "Start environment…"
                } else {
                    "Stop environment…"
                },
                env_act(if stopped {
                    EnvAction::Start
                } else {
                    EnvAction::Stop
                }),
                None,
            ));
            let _ = sub.append(&b.item("Purge edge cache…", env_act(EnvAction::Purge), None));
            let top = menu.items().len();
            b.owned.push((top, app.account.clone()));
            let _ = menu.append(&sub);
            b.signature.push(format!("env:{}", env.id));
            b.rows.push((vec![top], row));
            for (i, card) in sub_cards {
                b.cards.push((vec![top, i], card));
            }
        }
    }

    if settings.show_spend && !s.usage.is_empty() {
        let _ = menu.append(&PredefinedMenuItem::separator());
        for (account, org, u) in &s.usage {
            b.owned.push((menu.items().len(), account.clone()));
            let mut caption = vec![];
            if let Some(bw) = u.bandwidth_pct {
                caption.push(format!("Bandwidth {bw}%"));
            }
            if let Some((used, total)) = u.credits {
                caption.push(format!(
                    "credits {} of {}",
                    api::fmt_money(used, &u.currency),
                    api::fmt_money(total, &u.currency)
                ));
            }
            if let Some((threshold, _)) = u.alert {
                caption.push(format!(
                    "alert at {}",
                    api::fmt_money(threshold, &u.currency)
                ));
            }
            let title = if s.usage.len() > 1 && active.is_none() {
                format!("{org} · spend this period")
            } else {
                "Spend this period".into()
            };
            b.card_in_menu(
                &menu,
                Card::Spend {
                    title,
                    value: api::fmt_money(u.spend_cents, &u.currency),
                    caption: if caption.is_empty() {
                        "Current billing period".into()
                    } else {
                        caption.join(" · ")
                    },
                    meter: u
                        .bandwidth_pct
                        .and_then(|p| charts::meter(p as f64 / 100.0, chart_w(), 6.0)),
                },
            );
        }
    }

    let _ = menu.append(&PredefinedMenuItem::separator());
    if let Some((version, installing)) = update {
        let _ = if installing {
            menu.append(&b.item(&format!("Installing LC Menubar {version}…"), None, None))
        } else {
            menu.append(&b.item(
                &format!("Update to LC Menubar {version}…"),
                Some(Action::Update),
                None,
            ))
        };
    }
    let _ = menu.append(&b.item("Refresh", Some(Action::Refresh), "CmdOrCtrl+R".parse().ok()));
    let _ = menu.append(&b.item(
        "Open Laravel Cloud",
        Some(Action::OpenUrl(CLOUD_URL.into())),
        None,
    ));
    let _ = menu.append(&b.item(
        "Settings…",
        Some(Action::Window(WinKey::Settings)),
        "CmdOrCtrl+,".parse().ok(),
    ));
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&b.item("About LC Menubar", Some(Action::About), None));
    let _ = menu.append(&b.item(
        "Quit LC Menubar",
        Some(Action::Quit),
        "CmdOrCtrl+Q".parse().ok(),
    ));

    let hidden = b
        .owned
        .iter()
        .map(|(i, account)| (vec![*i], active.as_ref().is_some_and(|a| a != account)))
        .collect();
    Built {
        menu,
        actions: b.actions,
        cards: b.cards,
        rows: b.rows,
        signature: b.signature,
        hidden,
        org_ids,
    }
}

/// Writes `icon_16x16.png` … `icon_512x512@2x.png` for `iconutil`.
fn write_iconset(dir: &str) {
    std::fs::create_dir_all(dir).expect("iconset dir");
    for pt in [16u32, 32, 128, 256, 512] {
        for (scale, suffix) in [(1u32, ""), (2, "@2x")] {
            let png = charts::app_icon_png(pt * scale).expect("icon");
            let path = format!("{dir}/icon_{pt}x{pt}{suffix}.png");
            std::fs::write(&path, png).expect("write icon");
        }
    }
}

/// Sample data for `--demo` and `--preview-cards`.
fn demo_snapshot(period: Period) -> Snapshot {
    use model::{Application, Deployment, Metric, Series};
    // Longer ranges show slower, broader waves so switching visibly changes the charts.
    let stretch = match period {
        Period::H1 => 1.0,
        Period::H6 => 2.2,
        Period::H24 => 4.0,
        Period::D7 => 7.0,
    };
    let wave = |base: f64, amp: f64, seed: f64| -> Vec<f64> {
        (0..60)
            .map(|i| {
                let x = i as f64 * stretch;
                (base + amp * (x / 7.0 + seed).sin() + amp * 0.45 * (x / 2.3 + seed * 2.0).cos())
                    .max(0.0)
            })
            .collect()
    };
    let metrics = |cpu: f64, mem: f64, req: f64, seed: f64| Metrics {
        period,
        cpu: Some(Metric {
            series: vec![Series {
                label: "web".into(),
                points: wave(cpu, cpu * 0.4, seed),
                average: Some(cpu),
            }],
            total: None,
        }),
        memory: Some(Metric {
            series: vec![Series {
                label: "".into(),
                points: wave(mem, mem * 0.05, seed),
                average: Some(mem),
            }],
            total: None,
        }),
        requests: Some(Metric {
            series: vec![
                Series {
                    label: "2xx".into(),
                    points: wave(req, req * 0.4, seed),
                    average: Some(req * 60.0),
                },
                Series {
                    label: "5xx".into(),
                    points: (0..60)
                        .map(|i| {
                            if (i + seed as usize) % 17 == 3 {
                                req * 0.02
                            } else {
                                0.0
                            }
                        })
                        .collect(),
                    average: Some(req * 0.2),
                },
            ],
            total: Some(req * 60.2),
        }),
        replicas: Some(Metric {
            series: vec![Series {
                label: "".into(),
                points: vec![2.0, 3.0, 2.0],
                average: None,
            }],
            total: None,
        }),
        workers: Some(Metric {
            series: vec![Series {
                label: "".into(),
                points: vec![8.0],
                average: None,
            }],
            total: None,
        }),
    };
    let dep = |status: &str, branch: &str, msg: &str, mins: i64| Deployment {
        id: format!("d-{branch}-{mins}"),
        status: status.into(),
        branch: branch.into(),
        commit_hash: "8f3c2a91d4e2".into(),
        commit_message: msg.into(),
        commit_author: Some("Mia Lorenz".into()),
        failure_reason: (status == "build.failed")
            .then(|| "npm run build exited with code 1".into()),
        started_at: Some(Utc::now() - chrono::Duration::minutes(mins + 3)),
        finished_at: (status != "build.running")
            .then(|| Utc::now() - chrono::Duration::minutes(mins)),
    };
    let env = |id: &str, name: &str, d: Deployment, m: Option<Metrics>| Environment {
        id: id.into(),
        name: name.into(),
        status: "running".into(),
        domain: Some(format!("{name}.example.laravel.cloud")),
        recent_branches: vec![d.branch.clone(), "release/2.4".into()],
        latest: Some(d),
        metrics: m,
    };
    let usage = api::Usage {
        currency: "USD".into(),
        spend_cents: 4_812,
        bandwidth_pct: Some(42),
        credits: None,
        alert: Some((10_000, 52)),
        applications: vec![],
    };
    Snapshot {
        apps: vec![
            Application {
                id: "app1".into(),
                account: "demo".into(),
                org: "Northwind".into(),
                name: "storefront".into(),
                default_branch: Some("main".into()),
                environments: vec![
                    env(
                        "e1",
                        "production",
                        dep(
                            "deployment.succeeded",
                            "main",
                            "Cache product listings per locale",
                            14,
                        ),
                        Some(metrics(23.0, 61.0, 410.0, 0.0)),
                    ),
                    env(
                        "e2",
                        "staging",
                        dep(
                            "build.running",
                            "feature/gift-card-checkout",
                            "Add gift card checkout",
                            1,
                        ),
                        Some(metrics(6.0, 38.0, 12.0, 1.3)),
                    ),
                ],
            },
            Application {
                id: "app2".into(),
                account: "demo".into(),
                org: "Northwind".into(),
                name: "admin-api".into(),
                default_branch: None,
                environments: vec![env(
                    "e3",
                    "production",
                    dep("build.failed", "main", "Upgrade to Laravel 13", 52),
                    Some(metrics(0.4, 18.0, 35.0, 2.1)),
                )],
            },
            Application {
                id: "app3".into(),
                account: "demo2".into(),
                org: "Acme".into(),
                name: "marketing-site".into(),
                default_branch: None,
                environments: vec![env(
                    "e4",
                    "production",
                    dep("deployment.succeeded", "main", "New pricing page", 180),
                    Some(metrics(11.0, 44.0, 220.0, 0.7)),
                )],
            },
        ],
        error: None,
        fetched_at: Some(Utc::now()),
        hidden: Default::default(),
        usage: vec![
            ("demo".into(), "Northwind".into(), usage.clone()),
            (
                "demo2".into(),
                "Acme".into(),
                api::Usage {
                    spend_cents: 1_207,
                    bandwidth_pct: Some(9),
                    ..usage
                },
            ),
        ],
        org_errors: vec![],
    }
}

/// Development helper: render sample cards to PNG files in `dir`.
fn preview_cards(dir: &str) {
    use model::{Metric, Series};
    let wave = |base: f64, amp: f64, n: usize| -> Vec<f64> {
        (0..n)
            .map(|i| {
                (base + amp * ((i as f64) / 7.0).sin() + amp * 0.4 * ((i as f64) / 2.3).cos())
                    .max(0.0)
            })
            .collect()
    };
    let m = Metrics {
        period: Period::H1,
        cpu: Some(Metric {
            series: vec![Series {
                label: "web".into(),
                points: wave(22.0, 9.0, 60),
                average: Some(21.4),
            }],
            total: None,
        }),
        memory: Some(Metric {
            series: vec![Series {
                label: "".into(),
                points: wave(61.0, 3.0, 60),
                average: Some(60.8),
            }],
            total: None,
        }),
        requests: Some(Metric {
            series: vec![
                Series {
                    label: "2xx".into(),
                    points: wave(420.0, 160.0, 60),
                    average: Some(24_812.0),
                },
                Series {
                    label: "5xx".into(),
                    points: (0..60)
                        .map(|i| if i % 17 == 3 { 6.0 } else { 0.0 })
                        .collect(),
                    average: Some(18.0),
                },
            ],
            total: Some(24_830.0),
        }),
        replicas: Some(Metric {
            series: vec![Series {
                label: "".into(),
                points: vec![2.0, 2.0, 3.0, 2.0],
                average: None,
            }],
            total: None,
        }),
        workers: Some(Metric {
            series: vec![Series {
                label: "".into(),
                points: vec![8.0],
                average: None,
            }],
            total: None,
        }),
    };
    let env = Environment {
        id: "e".into(),
        name: "production".into(),
        status: "running".into(),
        domain: None,
        latest: Some(model::Deployment {
            id: "d".into(),
            status: "deployment.succeeded".into(),
            branch: "main".into(),
            commit_hash: "8f3c2a91d4".into(),
            commit_message: "Cache product listings per locale".into(),
            commit_author: Some("Mia".into()),
            failure_reason: None,
            started_at: Some(Utc::now() - chrono::Duration::minutes(14)),
            finished_at: Some(Utc::now() - chrono::Duration::minutes(12)),
        }),
        metrics: None,
        recent_branches: vec![],
    };
    let mut cards = vec![deployment_card(&env, Utc::now())];
    cards.extend(metric_cards(&m, Period::H1));
    cards.push(Card::Spend {
        title: "Spend this period".into(),
        value: api::fmt_money(4_812, "USD"),
        caption: "Bandwidth 42% · alert at $100.00".into(),
        meter: charts::meter(0.42, chart_w(), 6.0),
    });
    let _ = std::fs::create_dir_all(dir);
    for dark in [false, true] {
        for (i, c) in cards.iter().enumerate() {
            #[cfg(target_os = "macos")]
            if let Some(png) = menu_views::render_png(c, dark) {
                let path = format!("{dir}/card-{i}-{}.png", if dark { "dark" } else { "light" });
                let _ = std::fs::write(&path, png);
                println!("{path}");
            }
            #[cfg(not(target_os = "macos"))]
            let _ = (i, c, dark);
        }
    }
    let (rgba, w, h) = charts::row_icon(charts::POSITIVE, Some(&wave(22.0, 9.0, 60)));
    let _ = (rgba, w, h);
}

/// Status-bar icon: a single filled dot in the state's color.
/// Menu bar icon: template cloud plus colored status dot.
fn set_status_icon(tray: &TrayIcon, light: Light) {
    let dot = match light {
        Light::Green => Some(charts::POSITIVE),
        Light::Yellow => Some(charts::WARNING),
        Light::Red => Some(charts::NEGATIVE),
        Light::Grey => None,
    };
    #[cfg(target_os = "macos")]
    if let Some(item) = tray.ns_status_item() {
        menu_views::set_status_icon(&item, dot);
        return;
    }
    let (rgba, w, h) = charts::status_icon(true, dot, false);
    if let Ok(icon) = Icon::from_rgba(rgba, w, h) {
        let _ = tray.set_icon(Some(icon));
    }
}
