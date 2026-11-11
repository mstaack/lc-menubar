//! Native windows (tao) hosting small HTML pages (WKWebView via wry): Settings,
//! environment variables, logs and commands. Pages talk back through
//! `window.ipc.postMessage(json)`; Rust calls `window.<fn>(json)` to update them.
//!
//! Visual style: graphite neutrals, three text roles, one accent, 4px grid.

use anyhow::Result;
use serde::Serialize;
use tao::dpi::LogicalSize;
use tao::event_loop::EventLoopWindowTarget;
use tao::window::{Window, WindowBuilder, WindowId};
use wry::{WebView, WebViewBuilder};

/// A tao window hosting one HTML page.
pub struct WebWindow {
    pub window: Window,
    pub webview: WebView,
}

impl WebWindow {
    pub fn open<T: 'static>(
        target: &EventLoopWindowTarget<T>,
        title: &str,
        size: (f64, f64),
        html: String,
        on_message: impl Fn(String) + 'static,
    ) -> Result<Self> {
        // Hidden until the page reports `ready` (see `show`), so it never shows
        // an empty frame while WebKit loads.
        let window = WindowBuilder::new()
            .with_title(title)
            .with_inner_size(LogicalSize::new(size.0, size.1))
            .with_min_inner_size(LogicalSize::new(560.0, 420.0))
            .with_visible(false)
            .build(target)?;
        let webview = WebViewBuilder::new()
            .with_html(html)
            // Pages are local HTML: never load anything else (a dropped link, a
            // redirect) where it would get `window.ipc`.
            .with_navigation_handler(|url| url.starts_with("about:") || url.starts_with("data:"))
            .with_ipc_handler(move |req| on_message(req.into_body()))
            .build(&window)?;
        Ok(Self { window, webview })
    }

    /// A hidden, minimal web view created at launch: it starts WebKit's helper
    /// processes, so the first real window opens without a cold start.
    pub fn warm<T: 'static>(target: &EventLoopWindowTarget<T>) -> Result<Self> {
        let window = WindowBuilder::new()
            .with_visible(false)
            .with_inner_size(LogicalSize::new(10.0, 10.0))
            .build(target)?;
        let webview = WebViewBuilder::new()
            .with_html("<!doctype html><html><body></body></html>")
            .with_navigation_handler(|url| url.starts_with("about:") || url.starts_with("data:"))
            .build(&window)?;
        Ok(Self { window, webview })
    }

    pub fn show(&self) {
        self.window.set_visible(true);
        self.window.set_focus();
    }

    pub fn id(&self) -> WindowId {
        self.window.id()
    }

    /// Call a JS function defined by the page with one JSON argument.
    pub fn call(&self, func: &str, arg: &impl Serialize) {
        let arg = serde_json::to_string(arg).unwrap_or_else(|_| "null".into());
        let _ = self
            .webview
            .evaluate_script(&format!("window.{func} && window.{func}({arg})"));
    }

    pub fn toast(&self, text: &str, error: bool) {
        self.call(
            "toast",
            &serde_json::json!({ "text": text, "error": error }),
        );
    }
}

/// Assemble a page: shared CSS + page body, with `init` embedded as JSON.
fn page(body: &str, init: &impl Serialize) -> String {
    // `</` inside a <script> block would end it early.
    let json = serde_json::to_string(init)
        .unwrap_or_else(|_| "null".into())
        .replace("</", "<\\/");
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><style>{BASE_CSS}</style></head>{}</html>",
        // Insert the shared script first: it contains the INIT placeholder.
        body.replace("/*COMMON_JS*/", COMMON_JS)
            .replace("/*INIT*/null", &json)
    )
}

pub fn settings_html(init: &impl Serialize) -> String {
    page(SETTINGS_BODY, init)
}
pub fn vars_html(init: &impl Serialize) -> String {
    page(VARS_BODY, init)
}
pub fn logs_html(init: &impl Serialize) -> String {
    page(LOGS_BODY, init)
}
pub fn commands_html(init: &impl Serialize) -> String {
    page(COMMANDS_BODY, init)
}
pub fn update_html(init: &impl Serialize) -> String {
    page(UPDATE_BODY, init)
}

/// Standard base64, for embedding small images as `data:` URLs.
pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

const UPDATE_BODY: &str = r##"<body>
<style>
body{display:flex;flex-direction:column}
.uhead{display:flex;align-items:center;gap:16px;padding:24px 28px 18px}
.uhead img{width:64px;height:64px;flex:none}
.uhead h1{font-size:18px;line-height:24px}
.uhead p{color:var(--text-2);margin:2px 0 0}
.unotes{flex:1;overflow:auto;padding:0 28px 20px}
.unotes .rel-head{margin-top:4px}
footer.ubar{display:flex;align-items:center;gap:8px;padding:12px 20px;border-top:1px solid var(--line);background:var(--bg)}
</style>
<div class="uhead"><img id="icon" alt=""><div><h1 id="title"></h1><p id="sub"></p></div></div>
<div class="unotes" id="notes"></div>
<footer class="ubar">
  <button class="quiet" id="page">Release page</button>
  <span class="spacer"></span>
  <button id="later">Later</button>
  <button class="primary" id="install"></button>
</footer>
<script>
/*COMMON_JS*/
$('#icon').src = INIT.icon;
$('#title').textContent = `LC Menubar ${INIT.version} is available`;
$('#sub').textContent = INIT.canInstall
  ? `You have ${INIT.current}. LC Menubar restarts after the update.`
  : `You have ${INIT.current}. Download the new version from the release page.`;
if (INIT.groups.length) $('#notes').append(renderRelease({version: "What's new", groups: INIT.groups}, null));
$('#install').textContent = INIT.canInstall ? 'Install and restart' : 'Open release page';
$('#install').onclick = () => { $('#install').disabled = true; send({type: 'install'}); };
$('#later').onclick = () => send({type: 'close'});
$('#page').onclick = () => send({type: 'page'});
if (!INIT.canInstall) $('#page').style.display = 'none';
document.addEventListener('keydown', e => { if (e.key === 'Escape') send({type: 'close'}); });
</script></body>"##;

const BASE_CSS: &str = r#"
:root{
  --bg:#fafafa;--sidebar:#f4f4f5;--card:#ffffff;--raised:#f4f4f5;--deep:#f4f4f5;
  --line:rgba(23,23,23,.08);--line-strong:rgba(23,23,23,.14);
  --text:#171717;--text-2:#525252;--text-3:#737373;
  --accent:#2f6bc4;--accent-ink:#ffffff;--accent-soft:rgba(47,107,196,.10);
  --pos:#16a34a;--warn:#b7791f;--neg:#dc2626;
  --btn:#171717;--btn-ink:#fafafa;
  --r-ctl:8px;--r-card:12px;
  --mono:ui-monospace,"SF Mono",SFMono-Regular,Menlo,monospace;
}
@media (prefers-color-scheme:dark){:root{
  --bg:#141415;--sidebar:#0f0f10;--card:#19191b;--raised:#232325;--deep:#0f0f10;
  --line:rgba(237,237,237,.08);--line-strong:rgba(237,237,237,.14);
  --text:#ededed;--text-2:#a1a1a6;--text-3:#85858b;
  --accent:#6aa0ef;--accent-ink:#0f0f10;--accent-soft:rgba(106,160,239,.12);
  --pos:#4cc38a;--warn:#e0a23a;--neg:#e5484d;
  --btn:#ededed;--btn-ink:#141415;
}}
*{box-sizing:border-box}
html,body{margin:0;height:100%;background:var(--bg);color:var(--text);
  font:13px/20px -apple-system,BlinkMacSystemFont,"SF Pro Text",sans-serif;
  -webkit-font-smoothing:antialiased;cursor:default;-webkit-user-select:none}
.num,.mono{font-variant-numeric:tabular-nums}
.mono{font-family:var(--mono);font-size:12px}
h1{font-size:20px;line-height:28px;font-weight:600;margin:0}
h2{font-size:13px;line-height:20px;font-weight:500;color:var(--text-2);margin:24px 0 8px}
p{margin:0}
.t2{color:var(--text-2)} .t3{color:var(--text-3)}
.card{background:var(--card);border:1px solid var(--line);border-radius:var(--r-card)}
.row{display:flex;align-items:center;gap:16px;min-height:48px;padding:8px 16px}
.row+.row{border-top:1px solid var(--line)}
.row .grow{flex:1;min-width:0}
.row .hint{display:block;color:var(--text-3);font-size:12px;line-height:16px}
input,select,textarea{font:inherit;color:var(--text);background:var(--raised);border:1px solid var(--line);
  border-radius:var(--r-ctl);padding:4px 8px;outline:none;-webkit-user-select:text}
input,select{height:28px}
input:focus,select:focus,textarea:focus{border-color:var(--accent);box-shadow:0 0 0 3px var(--accent-soft)}
button{font:inherit;height:28px;padding:0 12px;border-radius:var(--r-ctl);border:1px solid var(--line-strong);
  background:var(--card);color:var(--text);transition:background-color .12s,opacity .12s,transform .08s}
button:hover{background:var(--raised)}
button:active{transform:scale(.98)}
button:disabled{opacity:.4;pointer-events:none}
button.primary{background:var(--btn);border-color:var(--btn);color:var(--btn-ink)}
button.primary:hover{opacity:.88}
button.quiet{border-color:transparent;background:transparent;color:var(--text-2)}
button.quiet:hover{background:var(--raised);color:var(--text)}
button.danger{color:var(--neg)}
button:focus-visible,input[type=checkbox]:focus-visible{outline:2px solid var(--accent);outline-offset:2px}
.switch{appearance:none;-webkit-appearance:none;width:32px;height:20px;border-radius:10px;background:var(--line-strong);
  position:relative;flex:none;transition:background-color .15s;margin:0;border:none}
.switch:before{content:"";position:absolute;width:16px;height:16px;left:2px;top:2px;border-radius:8px;background:#fff;
  box-shadow:0 1px 2px rgba(0,0,0,.25);transition:transform .15s}
.switch:checked{background:var(--accent)}
.switch:checked:before{transform:translateX(12px)}
.seg{display:inline-flex;background:var(--raised);border:1px solid var(--line);border-radius:var(--r-ctl);padding:2px;gap:2px}
.seg button{height:22px;border:none;background:transparent;color:var(--text-2);padding:0 10px;border-radius:6px}
.seg button.on{background:var(--card);color:var(--text);box-shadow:0 0 0 1px var(--line)}
.dot{display:inline-block;width:8px;height:8px;border-radius:4px;flex:none;background:var(--text-3)}
.dot.pos{background:var(--pos)} .dot.warn{background:var(--warn)} .dot.neg{background:var(--neg)}
.empty{padding:32px 16px;text-align:center;color:var(--text-3)}
.bar{display:flex;align-items:center;gap:8px;flex-wrap:wrap}
.spacer{flex:1}
footer.status{display:flex;align-items:center;gap:8px;padding:12px 24px;border-top:1px solid var(--line);
  color:var(--text-3);font-size:12px;min-height:52px;background:var(--bg)}
footer.status .msg{flex:1}
footer.status .msg.err{color:var(--neg)}
.rel-head{display:flex;align-items:center;gap:8px;margin:0 0 10px}
.rel+.rel{margin-top:32px}
#newslist{margin-top:28px}
.rel-head .v{font-size:15px;line-height:22px;font-weight:600;color:var(--text)}
.rel-head .d{color:var(--text-3);font-size:12px}
.badge{font-size:11px;line-height:18px;font-weight:600;padding:0 8px;border-radius:9px;background:var(--accent-soft);color:var(--accent)}
.rel .card{overflow:hidden;-webkit-user-select:text;cursor:auto}
.grp{padding:14px 18px 10px}
.grp+.grp{border-top:1px solid var(--line)}
.tag{display:inline-block;font-size:10.5px;line-height:18px;font-weight:700;letter-spacing:.06em;text-transform:uppercase;
  padding:0 8px;border-radius:5px;color:var(--text-2);background:var(--raised)}
.tag.added{color:var(--pos);background:color-mix(in srgb,var(--pos) 15%,transparent)}
.tag.changed{color:var(--accent);background:var(--accent-soft)}
.tag.fixed{color:var(--warn);background:color-mix(in srgb,var(--warn) 15%,transparent)}
.tag.removed,.tag.security,.tag.deprecated{color:var(--neg);background:color-mix(in srgb,var(--neg) 14%,transparent)}
.grp ul{list-style:none;margin:8px 0 0;padding:0}
.grp li{position:relative;padding-left:16px;margin:0 0 7px;line-height:20px;color:var(--text)}
.grp li:before{content:"";position:absolute;left:3px;top:8px;width:5px;height:5px;border-radius:3px;background:var(--line-strong)}
.rel code{font-family:var(--mono);font-size:11.5px;background:var(--raised);border:1px solid var(--line);border-radius:4px;padding:0 4px}
::-webkit-scrollbar{width:10px;height:10px}
::-webkit-scrollbar-thumb{background:var(--line-strong);border-radius:5px;border:2px solid var(--bg)}
"#;

const COMMON_JS: &str = r#"
const INIT = /*INIT*/null;
const send = m => window.ipc.postMessage(JSON.stringify(m));
const $ = q => document.querySelector(q);
const el = (tag, cls, text) => { const e = document.createElement(tag); if (cls) e.className = cls; if (text != null) e.textContent = text; return e; };
document.addEventListener('keydown', e => { if (e.metaKey && e.key === 'w') { e.preventDefault(); send({type: 'close'}); } });
// After the page's script has built it (a microtask: hidden views throttle timers).
queueMicrotask(() => send({type: 'ready'}));
// Changelog rendering (What's new, the update window): inline `code` becomes
// <code>, [text](url) keeps its text; groups get a coloured label.
function inline(text) {
  const f = document.createDocumentFragment();
  text.replace(/\[([^\]]+)\]\([^)]*\)/g, '$1').replace(/\*\*([^*]+)\*\*/g, '$1').split('`').forEach((part, i) =>
    f.append(i % 2 ? el('code', null, part) : document.createTextNode(part)));
  return f;
}
function renderGroups(groups) {
  const card = el('div', 'card');
  for (const [name, items] of groups) {
    if (!items.length) continue;
    const g = el('div', 'grp');
    if (name) g.append(el('span', 'tag ' + name.toLowerCase(), name));
    const ul = el('ul');
    items.forEach(it => { const li = el('li'); li.append(inline(it)); ul.append(li); });
    g.append(ul); card.append(g);
  }
  return card;
}
function renderRelease(r, badge) {
  const box = el('div', 'rel'), h = el('div', 'rel-head');
  h.append(el('span', 'v', r.version));
  if (badge) h.append(el('span', 'badge', badge));
  if (r.date) h.append(el('span', 'd', r.date));
  box.append(h, renderGroups(r.groups));
  return box;
}
"#;

const SETTINGS_BODY: &str = r##"<body>
<style>
body{display:flex}
nav{width:208px;flex:none;background:var(--sidebar);border-right:1px solid var(--line);padding:24px 12px;display:flex;flex-direction:column;gap:2px}
nav .brand{display:flex;align-items:center;gap:8px;padding:0 8px 16px;font-weight:600}
nav button{display:flex;align-items:center;gap:8px;width:100%;height:32px;border:none;background:transparent;color:var(--text-2);text-align:left;padding:0 8px;border-radius:var(--r-ctl)}
nav button:hover{background:var(--raised);color:var(--text)}
nav button.on{background:var(--card);color:var(--text);box-shadow:0 0 0 1px var(--line)}
nav .ver{margin-top:auto;padding:0 8px;color:var(--text-3);font-size:12px}
main{flex:1;min-width:0;display:flex;flex-direction:column}
.content{flex:1;overflow:auto;padding:32px 32px 24px}
.content section{display:none;max-width:620px}
.content section.on{display:block}
.content .lead{color:var(--text-2);margin:4px 0 0}
.org .name{font-weight:500}
.envapp{padding:12px 16px 4px;color:var(--text-3);font-size:12px}
.envrow{display:flex;align-items:center;gap:12px;min-height:40px;padding:4px 16px}
.envrow .grow{flex:1}
.project+.project{margin-top:12px}
.project .row .name{font-weight:500}
.project .row .hint{display:block}
.project .envrow{padding-left:28px;border-top:1px solid var(--line)}
.envrow.off{color:var(--text-3)}
.switch:disabled{opacity:.4}
.fav{display:flex;align-items:center;gap:8px;min-height:40px;padding:4px 8px 4px 16px}
.fav+.fav{border-top:1px solid var(--line)}
.fav .mono{flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
input.token{flex:1;font-family:var(--mono);font-size:12px}
footer.status:has(.msg:empty){display:none}
</style>
<nav>
  <div class="brand"><span class="dot pos"></span>LC Menubar</div>
  <button data-tab="general" class="on">General</button>
  <button data-tab="orgs">Organizations</button>
  <button data-tab="notify">Notifications</button>
  <button data-tab="menu">Menu and metrics</button>
  <button data-tab="envs">Projects</button>
  <button data-tab="commands">Commands</button>
  <button data-tab="news">What's new</button>
  <div class="ver" id="ver"></div>
</nav>
<main>
<div class="content">
  <section id="general" class="on">
    <h1>General</h1>
    <p class="lead">How LC Menubar starts and how often it checks Laravel Cloud</p>
    <h2>Startup</h2>
    <div class="card">
      <div class="row"><div class="grow">Open at login<span class="hint">Adds a LaunchAgent that starts this binary when you log in</span></div><input type="checkbox" class="switch" data-k="start_at_login"></div>
    </div>
    <h2>Refresh</h2>
    <div class="card">
      <div class="row"><div class="grow">Check for changes every<span class="hint">While a deployment runs, LC Menubar checks every 10 seconds</span></div>
        <select data-k="refresh_secs" data-num><option value="30">30 seconds</option><option value="60">1 minute</option><option value="120">2 minutes</option><option value="300">5 minutes</option></select></div>
    </div>
    <h2>Updates</h2>
    <div class="card">
      <div class="row"><div class="grow">Check for updates automatically<span class="hint">New versions appear at the bottom of the menu, installing one restarts LC Menubar</span></div><input type="checkbox" class="switch" data-k="check_updates"></div>
      <div class="row"><div class="grow">Check now<span class="hint" id="curver"></span></div><button id="checkupd">Check for updates</button></div>
    </div>
    <h2>About</h2>
    <div class="card">
      <div class="row"><div class="grow">Made by Max<span class="hint">100% vibe-coded with Claude</span></div><button id="author">github.com/mstaack</button></div>
      <div class="row t3" style="min-height:40px">LC Menubar is an independent project. It is not affiliated with, endorsed or sponsored by Laravel Holdings Inc. Laravel is a trademark of Laravel Holdings Inc.</div>
    </div>
  </section>

  <section id="orgs">
    <h1>Organizations</h1>
    <p class="lead">Each organization uses its own API token, stored in your macOS Keychain</p>
    <h2>Connected</h2>
    <div class="card" id="orglist"></div>
    <h2>Add an organization</h2>
    <div class="card">
      <div class="row"><input class="token" id="token" type="password" placeholder="API token" autocomplete="off"><button class="primary" id="addorg">Connect</button></div>
      <div class="row t3" style="min-height:40px">Create a token in the Laravel Cloud dashboard under your organization's API settings</div>
    </div>
  </section>

  <section id="notify">
    <h1>Notifications</h1>
    <p class="lead">Which deployment events show a macOS notification</p>
    <h2>Deployments</h2>
    <div class="card">
      <div class="row"><div class="grow">Started</div><input type="checkbox" class="switch" data-k="notify_started"></div>
      <div class="row"><div class="grow">Succeeded</div><input type="checkbox" class="switch" data-k="notify_succeeded"></div>
      <div class="row"><div class="grow">Failed</div><input type="checkbox" class="switch" data-k="notify_failed"></div>
      <div class="row"><div class="grow">Cancelled</div><input type="checkbox" class="switch" data-k="notify_cancelled"></div>
    </div>
  </section>

  <section id="menu">
    <h1>Menu and metrics</h1>
    <p class="lead">What the menu shows for each environment</p>
    <h2>Metrics</h2>
    <div class="card">
      <div class="row"><div class="grow">Show metrics<span class="hint">CPU, memory, requests, replicas and web workers in each environment's submenu</span></div><input type="checkbox" class="switch" data-k="metrics_enabled"></div>
      <div class="row"><div class="grow">Time range</div>
        <select data-k="metrics_period"><option value="1h">Last hour</option><option value="6h">Last 6 hours</option><option value="24h">Last 24 hours</option><option value="7d">Last 7 days</option></select></div>
      <div class="row"><div class="grow">CPU next to the environment name</div><input type="checkbox" class="switch" data-k="show_cpu_in_menu"></div>
      <div class="row"><div class="grow">Replica count next to the environment name</div><input type="checkbox" class="switch" data-k="show_replicas_in_menu"></div>
    </div>
    <h2>Spend</h2>
    <div class="card">
      <div class="row"><div class="grow">Show spend for the current billing period<span class="hint">Refreshed every 10 minutes</span></div><input type="checkbox" class="switch" data-k="show_spend"></div>
      <div class="row"><div class="grow">Show the total next to the status dot</div><input type="checkbox" class="switch" data-k="show_spend_in_menu_bar"></div>
    </div>
  </section>

  <section id="envs">
    <h1>Projects</h1>
    <p class="lead">Hidden projects and environments are not checked, not shown and do not change the status dot</p>
    <h2>Visible in the menu</h2>
    <div id="envlist"></div>
  </section>

  <section id="commands">
    <h1>Commands</h1>
    <p class="lead">Saved commands offered in the command runner of every environment</p>
    <h2>Saved commands</h2>
    <div class="card" id="favlist"></div>
    <h2>Add a command</h2>
    <div class="card"><div class="row"><input id="newfav" class="token" placeholder="php artisan …"><button id="addfav">Add</button></div></div>
  </section>

  <section id="news">
    <h1>What's new</h1>
    <p class="lead">Changes in each version of LC Menubar</p>
    <div id="newslist"></div>
  </section>
</div>
<footer class="status"><span class="msg" id="msg"></span></footer>
</main>
<script>
/*COMMON_JS*/
let init = INIT, s = init.settings, t;
const toast = (msg, err) => { const m = $('#msg'); m.textContent = msg; m.className = 'msg' + (err ? ' err' : ''); clearTimeout(t); if (!err) t = setTimeout(() => m.textContent = '', 2400); };
window.toast = o => toast(o.text, o.error);
$('#ver').textContent = 'Version ' + init.version;
$('#curver').textContent = 'You have version ' + init.version;
$('#checkupd').onclick = () => send({type: 'check_updates'});
$('#author').onclick = () => send({type: 'open_author'});

for (const r of init.changelog || []) {
  const badge = r.version === init.version ? 'Installed' : (r.version === 'Unreleased' ? 'Next' : null);
  $('#newslist').append(renderRelease(r, badge));
}

document.querySelectorAll('nav button[data-tab]').forEach(b => b.onclick = () => {
  document.querySelectorAll('nav button').forEach(x => x.classList.toggle('on', x === b));
  document.querySelectorAll('section').forEach(x => x.classList.toggle('on', x.id === b.dataset.tab));
});

function save() { send({type: 'save', settings: s}); }

function bindFields() {
  document.querySelectorAll('[data-k]').forEach(f => {
    const k = f.dataset.k;
    if (f.type === 'checkbox') f.checked = !!s[k]; else f.value = String(s[k]);
    f.onchange = () => { s[k] = f.type === 'checkbox' ? f.checked : (f.dataset.num !== undefined ? Number(f.value) : f.value); save(); };
  });
}

function renderOrgs() {
  const box = $('#orglist'); box.innerHTML = '';
  const rows = init.accounts.map(a => ({...a, env: false}));
  if (init.envAccount) rows.push({id: 'env', name: 'From LARAVEL_CLOUD_TOKEN', connected: true, env: true});
  if (!rows.length) { box.append(el('div', 'empty', 'No organization connected yet')); return; }
  for (const a of rows) {
    const r = el('div', 'row org');
    const d = el('span', 'dot ' + (a.connected ? 'pos' : 'neg'));
    const g = el('div', 'grow'); g.append(el('span', 'name', a.name), el('span', 'hint', a.env ? 'Read from the environment, not stored' : (a.connected ? 'Token in Keychain' : 'Token missing from Keychain')));
    r.append(d, g);
    if (!a.env) { const b = el('button', 'quiet danger', 'Remove'); b.onclick = () => send({type: 'removeAccount', id: a.id}); r.append(b); }
    box.append(r);
  }
}
$('#addorg').onclick = () => { const v = $('#token').value.trim(); if (!v) return toast('Paste a token first', true); send({type: 'addAccount', token: v}); $('#token').value = ''; };
$('#token').onkeydown = e => { if (e.key === 'Enter') $('#addorg').click(); };

function toggle(list, id, show) {
  const h = new Set(s[list] || []); show ? h.delete(id) : h.add(id); s[list] = [...h];
}
function renderEnvs() {
  const box = $('#envlist'); box.innerHTML = '';
  if (!init.envs.length) { const c = el('div', 'card'); c.append(el('div', 'empty', 'Projects appear here once LC Menubar has loaded them')); box.append(c); return; }
  const hiddenEnvs = new Set(s.hidden_envs), hiddenApps = new Set(s.hidden_apps || []);
  const apps = [];
  for (const e of init.envs) {
    let a = apps.find(x => x.id === e.appId);
    if (!a) apps.push(a = {id: e.appId, name: e.app, org: e.org, envs: []});
    a.envs.push(e);
  }
  for (const a of apps) {
    const card = el('div', 'card project'), shown = !hiddenApps.has(a.id);
    const head = el('div', 'row'); const g = el('div', 'grow');
    g.append(el('span', 'name', a.name));
    if (a.org && init.accounts.length > 1) g.append(el('span', 'hint', a.org));
    const cb = el('input', 'switch'); cb.type = 'checkbox'; cb.checked = shown;
    cb.onchange = () => { toggle('hidden_apps', a.id, cb.checked); save(); renderEnvs(); };
    head.append(g, cb); card.append(head);
    for (const e of a.envs) {
      const r = el('div', 'envrow' + (shown ? '' : ' off')); const name = el('span', 'grow', e.name);
      const ecb = el('input', 'switch'); ecb.type = 'checkbox'; ecb.checked = !hiddenEnvs.has(e.id); ecb.disabled = !shown;
      ecb.onchange = () => { toggle('hidden_envs', e.id, ecb.checked); save(); };
      r.append(name, ecb); card.append(r);
    }
    box.append(card);
  }
}

function renderFavs() {
  const box = $('#favlist'); box.innerHTML = '';
  if (!s.favorite_commands.length) { box.append(el('div', 'empty', 'No saved commands')); return; }
  s.favorite_commands.forEach((c, i) => {
    const r = el('div', 'fav'); r.append(el('span', 'mono', c));
    const b = el('button', 'quiet', 'Remove'); b.onclick = () => { s.favorite_commands.splice(i, 1); save(); renderFavs(); };
    r.append(b); box.append(r);
  });
}
$('#addfav').onclick = () => { const v = $('#newfav').value.trim(); if (!v) return; if (!s.favorite_commands.includes(v)) s.favorite_commands.push(v); $('#newfav').value = ''; save(); renderFavs(); };
$('#newfav').onkeydown = e => { if (e.key === 'Enter') $('#addfav').click(); };

window.setInit = i => { init = i; s = i.settings; render(); };
function render() { bindFields(); renderOrgs(); renderEnvs(); renderFavs(); }
render();
if (!init.accounts.length && !init.envAccount) document.querySelector('nav button[data-tab="orgs"]').click();
if (init.tab) document.querySelector(`nav button[data-tab="${init.tab}"]`)?.click();
document.addEventListener('keydown', e => { if (e.key === 'Escape') send({type: 'close'}); });
</script></body>"##;

const VARS_BODY: &str = r##"<body>
<style>
body{display:flex;flex-direction:column}
header{padding:24px 24px 12px}
header p{color:var(--text-2);margin-top:4px}
.tools{padding:0 24px 12px}
#q{width:220px}
#list{flex:1;overflow:auto;padding:0 24px 24px}
table{width:100%;border-collapse:separate;border-spacing:0;background:var(--card);border:1px solid var(--line);border-radius:var(--r-card);overflow:hidden}
td{padding:4px 8px;border-top:1px solid var(--line);height:40px}
tr:first-child td{border-top:none}
td.k{width:38%}
td.x{width:40px;text-align:right}
td input{width:100%;border-color:transparent;background:transparent;font-family:var(--mono);font-size:12px}
td input:hover{border-color:var(--line)}
tr.chg td{background:rgba(224,162,58,.10)} tr.new td{background:rgba(76,195,138,.10)}
#raw{width:100%;height:100%;min-height:320px;resize:none;font-family:var(--mono);font-size:12px;line-height:20px;padding:12px;background:var(--card)}
</style>
<header><h1 id="title"></h1><p>Environment variables. Changes apply on the next deployment</p></header>
<div class="tools bar">
  <input id="q" placeholder="Filter keys">
  <button id="add">Add variable</button>
  <span class="spacer"></span>
  <div class="seg"><button id="m-table" class="on">Table</button><button id="m-raw">.env</button></div>
  <button id="reveal" class="quiet">Show values</button>
  <button id="reload" class="quiet">Reload</button>
</div>
<div id="list"><div class="empty">Loading</div></div>
<footer class="status">
  <span class="msg" id="msg"></span>
  <button id="deploy" style="display:none">Deploy now</button>
  <button id="revert" disabled>Revert</button>
  <button id="save" class="primary" disabled>Save changes</button>
</footer>
<script>
/*COMMON_JS*/
$('#title').textContent = INIT.title;
let original = null, rows = [], reveal = false, raw = false, armed = false;
function status(text, err) { const m = $('#msg'); m.textContent = text || ''; m.className = 'msg' + (err ? ' err' : ''); }
window.toast = o => status(o.text, o.error);
window.loaded = o => {
  original = o.vars; rows = o.vars.map(([k, v]) => ({k, v, ok: k, ov: v}));
  if (raw) $('#raw').value = toEnv(rows);
  render(); status(`${rows.length} variables`);
};
window.failed = o => { status(o.text, true); if (!original) $('#list').innerHTML = '<div class="empty">Could not load variables</div>'; busy(false); };
window.saved = o => { status(o.text); $('#deploy').style.display = ''; busy(false); };

const origMap = () => new Map(original || []);
function edited() {
  if (raw) { const om = origMap(); rows = parseEnv($('#raw').value).map(([k, v]) => ({k, v, ...(om.has(k) ? {ok: k, ov: om.get(k)} : {})})); }
  return rows.map(r => [r.k.trim(), r.v]);
}
function dirty() {
  if (!original) return false;
  const e = edited(); if (e.length !== original.length) return true;
  return e.some(([k, v], i) => original[i][0] !== k || original[i][1] !== v);
}
function refresh() { const d = dirty(); $('#save').disabled = !d; $('#revert').disabled = !d; }
function busy(b) { $('#save').disabled = b || !dirty(); $('#reload').disabled = b; }
function cls(r) { return r.ok === undefined ? 'new' : (r.k !== r.ok || r.v !== r.ov ? 'chg' : ''); }

function render() {
  const list = $('#list');
  if (raw) return refresh();
  const q = $('#q').value.trim().toUpperCase();
  const vis = rows.filter(r => !q || r.k.toUpperCase().includes(q));
  if (!vis.length) { list.innerHTML = ''; list.append(el('div', 'empty', rows.length ? 'No matches' : 'No variables yet')); return refresh(); }
  const t = el('table');
  for (const r of vis) {
    const tr = el('tr', cls(r));
    const k = el('input'); k.value = r.k; k.placeholder = 'KEY'; k.spellcheck = false;
    k.oninput = () => { r.k = k.value.toUpperCase().replace(/[^A-Z0-9_]/g, '_'); k.value = r.k; tr.className = cls(r); refresh(); };
    const v = el('input'); v.value = r.v; v.placeholder = 'value'; v.spellcheck = false; v.type = reveal ? 'text' : 'password';
    v.onfocus = () => v.type = 'text'; v.onblur = () => v.type = reveal ? 'text' : 'password';
    v.oninput = () => { r.v = v.value; tr.className = cls(r); refresh(); };
    const x = el('button', 'quiet', 'Delete'); x.onclick = () => { rows.splice(rows.indexOf(r), 1); render(); };
    const a = el('td', 'k'); a.append(k); const b = el('td'); b.append(v); const c = el('td', 'x'); c.append(x);
    tr.append(a, b, c); t.append(tr);
  }
  list.innerHTML = ''; list.append(t); refresh();
}
function needsQuote(v) { return /[\s#"'\\]/.test(v) || v === ''; }
function toEnv(rs) { return rs.map(r => `${r.k}=${needsQuote(r.v) ? '"' + r.v.replace(/\\/g, '\\\\').replace(/"/g, '\\"').replace(/\n/g, '\\n') + '"' : r.v}`).join('\n') + '\n'; }
function parseEnv(text) {
  const out = [];
  for (let line of text.split('\n')) {
    line = line.trim(); if (!line || line.startsWith('#')) continue;
    if (line.startsWith('export ')) line = line.slice(7);
    const i = line.indexOf('='); if (i < 1) continue;
    const k = line.slice(0, i).trim(); let v = line.slice(i + 1).trim();
    if (v.length >= 2 && v.startsWith('"') && v.endsWith('"')) v = v.slice(1, -1).replace(/\\(.)/g, (_, c) => c === 'n' ? '\n' : c);
    else if (v.length >= 2 && v.startsWith("'") && v.endsWith("'")) v = v.slice(1, -1);
    else { const h = v.indexOf(' #'); if (h >= 0) v = v.slice(0, h).trim(); }
    out.push([k, v]);
  }
  return out;
}
function setMode(r) {
  if (!original || r === raw) return;
  if (!r) { edited(); raw = false; $('#q').disabled = false; render(); }
  else { raw = true; $('#q').disabled = true; const ta = el('textarea'); ta.id = 'raw'; ta.spellcheck = false; ta.value = toEnv(rows); ta.oninput = refresh; $('#list').innerHTML = ''; $('#list').append(ta); ta.focus(); }
  $('#m-table').classList.toggle('on', !raw); $('#m-raw').classList.toggle('on', raw);
}
$('#m-table').onclick = () => setMode(false);
$('#m-raw').onclick = () => setMode(true);
$('#q').oninput = render;
$('#add').onclick = () => {
  if (raw) { const ta = $('#raw'); ta.value += (ta.value && !ta.value.endsWith('\n') ? '\n' : '') + 'NEW_KEY=\n'; ta.focus(); return refresh(); }
  $('#q').value = ''; rows.push({k: '', v: ''}); render();
  const inputs = document.querySelectorAll('td.k input'); inputs[inputs.length - 1].focus();
};
$('#reveal').onclick = () => { reveal = !reveal; $('#reveal').textContent = reveal ? 'Hide values' : 'Show values'; render(); };
$('#reload').onclick = () => {
  if (dirty() && !armed) { armed = true; status('Unsaved changes. Click Reload again to discard them', true); setTimeout(() => armed = false, 4000); return; }
  armed = false; status('Loading'); $('#deploy').style.display = 'none'; send({type: 'reload'});
};
$('#revert').onclick = () => window.loaded({vars: original});
$('#save').onclick = () => { const e = edited(); status('Saving'); busy(true); send({type: 'save', original, edited: e}); };
$('#deploy').onclick = () => send({type: 'deploy'});
document.addEventListener('keydown', e => { if (e.metaKey && e.key === 's') { e.preventDefault(); if (!$('#save').disabled) $('#save').click(); } });
</script></body>"##;

const LOGS_BODY: &str = r##"<body>
<style>
body{display:flex;flex-direction:column}
header{padding:24px 24px 12px;display:flex;align-items:baseline;gap:12px}
.tools{padding:0 24px 12px}
#q{width:260px}
#list{flex:1;overflow:auto;margin:0 24px;background:var(--card);border:1px solid var(--line);border-radius:var(--r-card)}
.log{display:grid;grid-template-columns:72px 56px 1fr;gap:12px;padding:4px 16px;border-top:1px solid var(--line);font-family:var(--mono);font-size:12px;line-height:20px;-webkit-user-select:text;cursor:text}
.log:first-child{border-top:none}
.log:hover{background:var(--raised)}
.log .time{color:var(--text-3);font-variant-numeric:tabular-nums}
.log .lvl{color:var(--text-3)}
.log.error .lvl{color:var(--neg)} .log.warning .lvl{color:var(--warn)}
.log .msg{white-space:pre-wrap;word-break:break-word;color:var(--text)}
.log .st{display:inline-block;width:32px}
.st.s5{color:var(--neg)} .st.s4{color:var(--warn)} .st.s2,.st.s3{color:var(--text-2)}
.log .detail{grid-column:3;color:var(--text-2);white-space:pre-wrap;margin:4px 0 8px;font-size:11px;line-height:16px}
.live-on{color:var(--pos)}
#more{margin:12px auto;display:block}
</style>
<header><h1 id="title"></h1><span class="t3">Logs</span></header>
<div class="tools bar">
  <input id="q" placeholder="Search">
  <div class="seg" id="type"><button data-v="" class="on">All</button><button data-v="application">Application</button><button data-v="access">Access</button></div>
  <div class="seg" id="level"><button data-v="" class="on">Any level</button><button data-v="error">Errors</button><button data-v="warning">Warnings</button></div>
  <span class="spacer"></span>
  <select id="range"><option value="15">Last 15 minutes</option><option value="60" selected>Last hour</option><option value="360">Last 6 hours</option><option value="1440">Last 24 hours</option><option value="4320">Last 3 days</option></select>
  <button id="live" class="quiet">Live</button>
</div>
<div id="list"><div class="empty">Loading</div></div>
<footer class="status"><span class="msg" id="msg"></span><button id="refresh" class="quiet">Refresh</button></footer>
<script>
/*COMMON_JS*/
$('#title').textContent = INIT.title;
let logs = [], cursor = null, kind = '', level = '', live = false, timer = null, loading = false, windowFrom = null;
const seen = new Set();
function status(t, err) { const m = $('#msg'); m.textContent = t || ''; m.className = 'msg' + (err ? ' err' : ''); }
window.toast = o => status(o.text, o.error);
const iso = d => d.toISOString().replace(/\.\d+Z$/, 'Z');
function fetchLogs(mode) {
  if (loading) return; loading = true;
  const to = new Date();
  let from;
  if (mode === 'append' && logs.length) from = new Date(Math.max(...logs.map(l => Date.parse(l.logged_at) || 0)) - 1000);
  else { from = new Date(to - Number($('#range').value) * 60000); windowFrom = from; }
  const q = {from: iso(mode === 'older' ? windowFrom : from), to: iso(to), query: $('#q').value, kind: kind || null, cursor: mode === 'older' ? cursor : null};
  if (mode !== 'append') status('Loading');
  send({type: 'fetch', mode, query: q});
}
const key = l => l.logged_at + '|' + l.message;
window.logsLoaded = o => {
  loading = false;
  const items = Array.isArray(o.data) ? o.data : [];
  if (o.mode === 'replace') { logs = []; seen.clear(); }
  for (const l of items) { const k = key(l); if (!seen.has(k)) { seen.add(k); logs.push(l); } }
  logs.sort((a, b) => (Date.parse(b.logged_at) || 0) - (Date.parse(a.logged_at) || 0));
  if (o.mode !== 'append') cursor = o.meta && o.meta.cursor || null;
  render();
};
window.logsFailed = o => { loading = false; status(o.text, true); if (!logs.length) $('#list').innerHTML = '<div class="empty">Could not load logs</div>'; };
function fmtTime(s) { const d = new Date(s); return isNaN(d) ? s : d.toLocaleTimeString([], {hour12: false}); }
function render() {
  const list = $('#list'); list.innerHTML = '';
  const vis = logs.filter(l => !level || l.level === level);
  if (!vis.length) { list.append(el('div', 'empty', 'No log entries in this range')); }
  for (const l of vis.slice(0, 2000)) {
    const r = el('div', 'log ' + (l.level || ''));
    r.append(el('span', 'time', fmtTime(l.logged_at)), el('span', 'lvl', l.level || ''));
    const m = el('span', 'msg');
    const d = l.data || {};
    if (l.type === 'access' && d.status) {
      const st = el('span', 'st s' + String(d.status)[0], String(d.status));
      m.append(st, document.createTextNode(` ${d.method || ''} ${d.path || ''}` + (d.duration_ms != null ? `  ${Math.round(d.duration_ms)} ms` : '')));
    } else m.textContent = l.message;
    r.append(m);
    r.onclick = () => {
      const open = r.querySelector('.detail'); if (open) return open.remove();
      if (window.getSelection().toString()) return;
      const det = el('div', 'detail'); det.textContent = l.type === 'access' ? JSON.stringify(d, null, 2) : (l.message + (Object.keys(d).length ? '\n\n' + JSON.stringify(d, null, 2) : ''));
      r.append(el('span'), el('span'), det);
    };
    list.append(r);
  }
  if (cursor) { const b = el('button', 'quiet', 'Load older'); b.id = 'more'; b.onclick = () => fetchLogs('older'); list.append(b); }
  status(`${vis.length} entries` + (live ? ' · live' : ''));
}
document.querySelectorAll('#type button').forEach(b => b.onclick = () => { document.querySelectorAll('#type button').forEach(x => x.classList.toggle('on', x === b)); kind = b.dataset.v; fetchLogs('replace'); });
document.querySelectorAll('#level button').forEach(b => b.onclick = () => { document.querySelectorAll('#level button').forEach(x => x.classList.toggle('on', x === b)); level = b.dataset.v; render(); });
$('#range').onchange = () => fetchLogs('replace');
let qt; $('#q').oninput = () => { clearTimeout(qt); qt = setTimeout(() => fetchLogs('replace'), 400); };
$('#refresh').onclick = () => fetchLogs('replace');
$('#live').onclick = () => {
  live = !live; $('#live').classList.toggle('live-on', live); $('#live').textContent = live ? 'Live on' : 'Live';
  clearInterval(timer); if (live) timer = setInterval(() => fetchLogs('append'), 5000); render();
};
fetchLogs('replace');
</script></body>"##;

const COMMANDS_BODY: &str = r##"<body>
<style>
body{display:flex;flex-direction:column}
header{padding:24px 24px 12px;display:flex;align-items:baseline;gap:12px}
.runner{padding:0 24px 12px}
.runner input{flex:1;font-family:var(--mono);font-size:12px}
.favs{padding:0 24px 12px;display:flex;flex-wrap:wrap;gap:8px}
.favs button{height:24px;font-family:var(--mono);font-size:11px;color:var(--text-2)}
.split{flex:1;display:flex;gap:16px;padding:0 24px 0;min-height:0}
.out{flex:1;display:flex;flex-direction:column;min-width:0;background:var(--card);border:1px solid var(--line);border-radius:var(--r-card);overflow:hidden}
.out .head{display:flex;align-items:center;gap:8px;padding:8px 16px;border-bottom:1px solid var(--line);min-height:40px}
.out .head .cmd{flex:1;font-family:var(--mono);font-size:12px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
pre{flex:1;margin:0;padding:12px 16px;overflow:auto;font-family:var(--mono);font-size:12px;line-height:18px;white-space:pre-wrap;word-break:break-word;background:var(--deep);-webkit-user-select:text;cursor:text}
.hist{width:260px;flex:none;overflow:auto;background:var(--card);border:1px solid var(--line);border-radius:var(--r-card)}
.hist .item{display:flex;gap:8px;align-items:center;padding:8px 12px;border-top:1px solid var(--line);cursor:pointer}
.hist .item:first-child{border-top:none}
.hist .item:hover{background:var(--raised)}
.hist .item.on{background:var(--raised)}
.hist .item .c{flex:1;min-width:0;font-family:var(--mono);font-size:11px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.hist .item .w{font-size:11px;color:var(--text-3);font-variant-numeric:tabular-nums}
.hist h3{font-size:12px;font-weight:500;color:var(--text-3);margin:0;padding:12px 12px 4px}
</style>
<header><h1 id="title"></h1><span class="t3">Commands</span></header>
<div class="runner bar"><input id="cmd" placeholder="php artisan …" spellcheck="false"><button id="star" class="quiet">Save</button><button id="run" class="primary">Run</button></div>
<div class="favs" id="favs"></div>
<div class="split">
  <div class="out">
    <div class="head"><span class="dot" id="sdot"></span><span class="cmd" id="scmd">No command selected</span><span class="t3 num" id="smeta"></span></div>
    <pre id="output"></pre>
  </div>
  <div class="hist" id="hist"><h3>Recent</h3><div class="empty">Loading</div></div>
</div>
<footer class="status"><span class="msg" id="msg"></span><button id="reload" class="quiet">Refresh history</button></footer>
<script>
/*COMMON_JS*/
$('#title').textContent = INIT.title;
let favs = INIT.favorites || [], history = [], current = null, running = false;
function status(t, err) { const m = $('#msg'); m.textContent = t || ''; m.className = 'msg' + (err ? ' err' : ''); }
window.toast = o => status(o.text, o.error);
function renderFavs() {
  const box = $('#favs'); box.innerHTML = '';
  for (const f of favs) { const b = el('button', '', f); b.onclick = () => { $('#cmd').value = f; $('#cmd').focus(); updateStar(); }; box.append(b); }
}
function updateStar() { $('#star').textContent = favs.includes($('#cmd').value.trim()) ? 'Saved' : 'Save'; }
$('#star').onclick = () => {
  const v = $('#cmd').value.trim(); if (!v) return;
  favs = favs.includes(v) ? favs.filter(f => f !== v) : [...favs, v];
  send({type: 'favorites', favorites: favs}); renderFavs(); updateStar();
};
$('#cmd').oninput = updateStar;
const stateOf = s => s === 'command.success' ? ['pos', 'Succeeded'] : s === 'command.failure' ? ['neg', 'Failed'] : ['warn', 'Running'];
function dur(a) { if (!a.started_at || !a.finished_at) return ''; const s = Math.max(0, (Date.parse(a.finished_at) - Date.parse(a.started_at)) / 1000); return s < 60 ? `${s.toFixed(1)} s` : `${Math.floor(s / 60)}m ${Math.round(s % 60)}s`; }
function show(c) {
  current = c; const a = c.attributes || {};
  const [cls, label] = stateOf(a.status);
  $('#sdot').className = 'dot ' + cls; $('#scmd').textContent = a.command || '';
  const meta = [label]; if (a.exit_code != null) meta.push('exit ' + a.exit_code); const d = dur(a); if (d) meta.push(d);
  $('#smeta').textContent = meta.join(' · ');
  let out = a.output || (a.status === 'command.success' || a.status === 'command.failure' ? '(no output)' : '');
  if (a.failure_reason) out += (out ? '\n\n' : '') + a.failure_reason;
  if (a.output_truncated) out += '\n\nOutput truncated. Full output: ' + (a.output_url || 'not available');
  $('#output').textContent = out;
  const p = $('#output'); p.scrollTop = p.scrollHeight;
  document.querySelectorAll('.hist .item').forEach(i => i.classList.toggle('on', i.dataset.id === c.id));
}
function renderHistory() {
  const box = $('#hist'); box.innerHTML = ''; box.append(el('h3', '', 'Recent'));
  if (!history.length) { box.append(el('div', 'empty', 'No commands yet')); return; }
  for (const c of history) {
    const a = c.attributes || {}; const it = el('div', 'item'); it.dataset.id = c.id;
    it.append(el('span', 'dot ' + stateOf(a.status)[0]), el('span', 'c', a.command || ''));
    const t = a.created_at ? new Date(a.created_at) : null;
    it.append(el('span', 'w', t ? t.toLocaleString([], {month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit', hour12: false}) : ''));
    it.onclick = () => show(c); box.append(it);
  }
  if (current) document.querySelectorAll('.hist .item').forEach(i => i.classList.toggle('on', i.dataset.id === current.id));
}
window.historyLoaded = o => { if (o.error) { status(o.error, true); history = []; } else history = o.items || []; renderHistory(); };
window.commandUpdate = c => {
  if (!c || !c.id) return;
  const i = history.findIndex(h => h.id === c.id); if (i >= 0) history[i] = c; else history.unshift(c);
  renderHistory(); show(c); status('Running');
};
window.commandDone = () => { running = false; $('#run').disabled = false; const a = (current && current.attributes) || {}; status(stateOf(a.status)[1]); };
window.commandFailed = o => { running = false; $('#run').disabled = false; status(o.text, true); };
window.commandWarning = o => status(o.text, true);
window.commandCancelled = () => { running = false; $('#run').disabled = false; status('Cancelled'); };
$('#run').onclick = () => { const v = $('#cmd').value.trim(); if (!v || running) return; running = true; $('#run').disabled = true; status('Waiting for confirmation'); send({type: 'run', command: v}); };
$('#cmd').onkeydown = e => { if (e.key === 'Enter') $('#run').click(); };
$('#reload').onclick = () => send({type: 'history'});
renderFavs(); $('#cmd').focus();
</script></body>"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_embed_init_data() {
        let init = serde_json::json!({ "title": "Shop </script> prod", "favorites": [] });
        for html in [
            settings_html(&init),
            vars_html(&init),
            logs_html(&init),
            commands_html(&init),
        ] {
            assert!(!html.contains("/*INIT*/"), "placeholder left in page");
            assert!(!html.contains("/*COMMON_JS*/"));
            assert!(html.contains(r#"const INIT = {"#));
            assert!(html.contains(r"Shop <\/script> prod"));
        }
    }
}
