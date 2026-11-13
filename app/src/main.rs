mod cloud;
mod translate;
mod warp;

use anyhow::{anyhow, Context, Result};
use cloud::{Claude, Cloud, CloudErr, Gemini, Usage};
use futures_util::{SinkExt, StreamExt};
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, Mutex, Notify};
use tokio_tungstenite::tungstenite::Message;
use translate::{Hit, Translator};

const CDP_PORT: u16 = 9333;
const MODEL_PORT: u16 = 8089;
const LOCAL_WORKERS: usize = 2;
const INJECT: &str = include_str!("inject.js");

struct Cdp {
    tx: mpsc::UnboundedSender<Message>,
    next: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<Value>>>,
}

impl Cdp {
    async fn call(&self, method: &str, params: Value, session: Option<&str>) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let mut msg = json!({"id": id, "method": method, "params": params});
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        self.tx.send(Message::Text(msg.to_string())).map_err(|_| anyhow!("devtools closed"))?;
        let v = tokio::time::timeout(Duration::from_secs(15), rx).await??;
        if let Some(e) = v.get("error") {
            return Err(anyhow!("{method}: {e}"));
        }
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }

    fn eval(&self, session: &str, expr: String) {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let msg = json!({"id": id, "method": "Runtime.evaluate", "params": {"expression": expr}, "sessionId": session});
        let _ = self.tx.send(Message::Text(msg.to_string()));
    }

    fn apply(&self, session: &str, map: &HashMap<String, String>) {
        if !map.is_empty() {
            self.eval(session, format!("window.__tbtr_apply&&window.__tbtr_apply({})", serde_json::to_string(map).unwrap()));
        }
    }

    fn draft(&self, session: &str, map: &HashMap<String, String>) {
        if !map.is_empty() {
            self.eval(session, format!("window.__tbtr_draft&&window.__tbtr_draft({})", serde_json::to_string(map).unwrap()));
        }
    }
}

#[derive(Default)]
struct Queue {
    hi: VecDeque<String>,
    lo: VecDeque<String>,
}

impl Queue {
    fn push(&mut self, s: &str, high: bool) {
        if high {
            if !self.hi.iter().any(|x| x == s) {
                self.hi.push_back(s.to_string());
            }
        } else if !self.lo.iter().any(|x| x == s) && !self.hi.iter().any(|x| x == s) {
            self.lo.push_back(s.to_string());
        }
    }
    fn len(&self) -> usize {
        self.hi.len() + self.lo.len()
    }
    fn pick_one(&mut self) -> Option<String> {
        if !self.hi.is_empty() {
            let n = self.hi.len().min(24);
            let i = (0..n).min_by_key(|&i| self.hi[i].chars().count()).unwrap();
            return self.hi.remove(i);
        }
        self.lo.pop_front()
    }
    fn pick_batch(&mut self, n: usize) -> Vec<String> {
        let mut out = Vec::new();
        while out.len() < n {
            match self.hi.pop_front().or_else(|| self.lo.pop_front()) {
                Some(s) => out.push(s),
                None => break,
            }
        }
        out
    }
}

struct State {
    tr: Translator,
    subs: HashMap<String, HashSet<String>>,
    private_sessions: HashSet<String>,
    private_strs: HashSet<String>,
    target_session: HashMap<String, String>,
    cloud_q: Queue,
    local_q: Queue,
    in_flight: HashSet<String>,
    cloud_off: Vec<Option<Instant>>,
    stats: (usize, usize),
    local: bool,
}

#[derive(Debug, PartialEq)]
enum Route {
    Cloud,
    Local,
    DraftOnly,
}

fn route(private: bool, cloud: bool, local: bool) -> Route {
    if !private && cloud {
        Route::Cloud
    } else if local {
        Route::Local
    } else {
        Route::DraftOnly
    }
}

impl State {
    fn active_cloud(&self) -> Option<usize> {
        let now = Instant::now();
        self.cloud_off.iter().position(|t| t.map_or(true, |t| now >= t))
    }
    fn cloud_available(&self) -> bool {
        self.active_cloud().is_some()
    }
}

type Shared = Arc<Mutex<State>>;

fn wanted(info: &Value) -> bool {
    let t = info["type"].as_str().unwrap_or("");
    let url = info["url"].as_str().unwrap_or("");
    (t == "page" || t == "iframe")
        && !url.starts_with("devtools://")
        && !url.starts_with("chrome")
        && !url.is_empty()
        && url != "about:blank"
}

fn is_private(url: &str) -> bool {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)buyertrade|trade\.|/order|orderdetail|address|deliver|refund|logistics|wuliu|/im/|chat|wangwang|alipay|login|passport|member1?\.|myaccount|i\.taobao\.com/my_|cashier|pay\.|purchaseList|loginPop").unwrap()
    })
    .is_match(url)
}

async fn setup_session(cdp: Arc<Cdp>, state: Shared, target: String, session: String, url: String) {
    let private = is_private(&url);
    {
        let mut st = state.lock().await;
        st.target_session.insert(target, session.clone());
        if private {
            st.private_sessions.insert(session.clone());
        }
    }
    let r = async {
        cdp.call("Runtime.enable", json!({}), Some(&session)).await?;
        cdp.call("Runtime.addBinding", json!({"name": "__tbtr_send"}), Some(&session)).await?;
        let _ = cdp.call("Page.addScriptToEvaluateOnNewDocument", json!({"source": INJECT}), Some(&session)).await;
        cdp.call("Runtime.evaluate", json!({"expression": INJECT}), Some(&session)).await?;
        anyhow::Ok(())
    }
    .await;
    match r {
        Ok(()) => println!("[+]{} {}", if private { " (только локально)" } else { "" }, short(&url)),
        Err(e) => eprintln!("[!] {}: {e}", short(&url)),
    }
}

fn short(u: &str) -> String {
    let u = u.split('?').next().unwrap_or(u);
    u.chars().take(90).collect()
}

fn cut(s: &str, n: usize) -> String {
    let c: String = s.chars().take(n).collect();
    if s.chars().count() > n { c + "…" } else { c }
}

async fn on_request(state: &Shared, cdp: &Arc<Cdp>, wake: &Arc<Notify>, session: &str, payload: &str) {
    let Ok(v) = serde_json::from_str::<Value>(payload) else { return };
    let mut ready = HashMap::new();
    let mut drafts = HashMap::new();
    let mut st = state.lock().await;
    let private = st.private_sessions.contains(session);
    let way = route(private, st.cloud_available(), st.local);
    let mut added = false;
    for (list, high) in [("hi", true), ("lo", false)] {
        for s in v[list].as_array().into_iter().flatten().filter_map(|x| x.as_str()) {
            match st.tr.fast(s) {
                Hit::Fast(t) => {
                    ready.insert(s.to_string(), t);
                }
                Hit::Miss => {
                    if let Some(d) = st.tr.draft(s) {
                        drafts.insert(s.to_string(), d);
                    }
                    st.subs.entry(s.to_string()).or_default().insert(session.to_string());
                    if private {
                        st.private_strs.insert(s.to_string());
                    }
                    if st.in_flight.contains(s) {
                        continue;
                    }
                    match way {
                        Route::Cloud => st.cloud_q.push(s, high),
                        Route::Local => st.local_q.push(s, high),
                        Route::DraftOnly => continue,
                    }
                    added = true;
                }
            }
        }
    }
    drop(st);
    cdp.apply(session, &ready);
    cdp.draft(session, &drafts);
    if added {
        wake.notify_waiters();
    }
}

async fn deliver(state: &Shared, cdp: &Arc<Cdp>, results: Vec<(String, String)>, ms: u128, src: &str) {
    let mut per_session: HashMap<String, HashMap<String, String>> = HashMap::new();
    {
        let mut st = state.lock().await;
        for (zh, ru) in &results {
            st.tr.remember(zh, ru, ms, src);
            st.in_flight.remove(zh);
            for s in st.subs.remove(zh).unwrap_or_default() {
                per_session.entry(s).or_default().insert(zh.clone(), ru.clone());
            }
        }
        if src == "cloud" { st.stats.0 += results.len() } else { st.stats.1 += results.len() }
    }
    for (s, map) in per_session {
        cdp.apply(&s, &map);
    }
}

async fn cloud_worker(state: Shared, cdp: Arc<Cdp>, wake: Arc<Notify>, cloud: Arc<Cloud>, rank: usize, usage: Arc<std::sync::Mutex<Usage>>) {
    let pace = cloud.pace();
    let mut last = Instant::now() - pace.gap;
    loop {
        let batch = {
            let mut st = state.lock().await;
            if st.active_cloud() != Some(rank) {
                None
            } else {
                if st.cloud_q.len() == 0 && st.local_q.len() > 0 {
                    let st = &mut *st;
                    for (from, high) in [(&mut st.local_q.hi, true), (&mut st.local_q.lo, false)] {
                        let mut keep = VecDeque::new();
                        while let Some(x) = from.pop_front() {
                            if st.cloud_q.len() < pace.batch && !st.private_strs.contains(&x) {
                                st.cloud_q.push(&x, high);
                            } else {
                                keep.push_back(x);
                            }
                        }
                        *from = keep;
                    }
                }
                let b: Vec<String> = st.cloud_q.pick_batch(pace.batch).into_iter().filter(|s| matches!(st.tr.fast(s), Hit::Miss)).collect();
                if b.is_empty() {
                    None
                } else {
                    for s in &b {
                        st.in_flight.insert(s.clone());
                    }
                    let terms = st.tr.terms_for(&b);
                    Some((b, terms))
                }
            }
        };
        let Some((batch, terms)) = batch else {
            let _ = tokio::time::timeout(Duration::from_millis(500), wake.notified()).await;
            tokio::time::sleep(Duration::from_millis(150)).await;
            continue;
        };
        let wait = pace.gap.saturating_sub(last.elapsed());
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
        last = Instant::now();
        let t = Instant::now();
        usage.lock().unwrap().add(batch.len());
        match cloud.translate(&batch, &terms).await {
            Ok(done) => {
                let ms = t.elapsed().as_millis();
                let line = {
                    let mut u = usage.lock().unwrap();
                    u.spend(done.usd);
                    u.line(cloud.name())
                };
                let left = state.lock().await.cloud_q.len();
                println!("☁ {:>5}ms {:>2} строк  {line} (в очереди {left})   напр.: {} → {}", ms, batch.len(), cut(&batch[0], 16), cut(&done.lines[0], 36));
                let res: Vec<(String, String)> = batch.into_iter().zip(done.lines).map(|(z, r)| (z, r.trim().to_string())).collect();
                deliver(&state, &cdp, res, ms, "cloud").await;
            }
            Err(e) => {
                let mut st = state.lock().await;
                match e {
                    CloudErr::Busy => {
                        for s in batch.iter().rev() {
                            st.in_flight.remove(s);
                            st.cloud_q.hi.push_front(s.clone());
                        }
                        drop(st);
                        if pace.busy_wait >= Duration::from_secs(5) {
                            println!("☁ {} занят, подожду…", cloud.name());
                        }
                        tokio::time::sleep(pace.busy_wait).await;
                        continue;
                    }
                    CloudErr::Off(ref msg) => {
                        let pause = match &*cloud {
                            Cloud::Gemini(_) => usage.lock().unwrap().until_reset() + Duration::from_secs(120),
                            Cloud::Claude(_) => Duration::from_secs(1800),
                        };
                        st.cloud_off[rank] = Some(Instant::now() + pause);
                        let next = match route(false, st.cloud_available(), st.local) {
                            Route::Cloud => "запасное облако",
                            Route::Local => "RTX",
                            Route::DraftOnly => "только словарь",
                        };
                        println!("☁ {} выключен на {} мин, дальше переводит {next}. Ответ: {}", cloud.name(), pause.as_secs() / 60, cut(msg, 120));
                    }
                    CloudErr::Other(ref err) => {
                        println!("☁ {}: ошибка, пачка {}: {}", cloud.name(), if st.local { "уйдёт на RTX" } else { "останется по словарю" }, cut(&err.to_string(), 120));
                    }
                }
                let retry_cloud = matches!(e, CloudErr::Off(_)) && st.cloud_available();
                for s in batch {
                    st.in_flight.remove(&s);
                    match route(false, retry_cloud, st.local) {
                        Route::Cloud => st.cloud_q.push(&s, true),
                        Route::Local => st.local_q.push(&s, true),
                        Route::DraftOnly => {}
                    }
                }
                if !st.cloud_available() && st.local {
                    let rest = st.cloud_q.pick_batch(usize::MAX);
                    for s in rest {
                        st.local_q.push(&s, false);
                    }
                }
                drop(st);
                wake.notify_waiters();
            }
        }
    }
}

async fn local_worker(state: Shared, cdp: Arc<Cdp>, wake: Arc<Notify>, id: usize) {
    loop {
        let job = {
            let mut st = state.lock().await;
            loop {
                match st.local_q.pick_one() {
                    Some(s) if st.tr.cache.contains_key(&s) || st.in_flight.contains(&s) => continue,
                    Some(s) => {
                        st.in_flight.insert(s.clone());
                        break Some((st.tr.prompt(&s), st.tr.client(), s));
                    }
                    None => break None,
                }
            }
        };
        let Some((prompt, (http, url), zh)) = job else {
            let _ = tokio::time::timeout(Duration::from_millis(500), wake.notified()).await;
            continue;
        };
        match Translator::model(&http, &url, prompt, &zh).await {
            Ok((ru, ms)) => {
                let left = state.lock().await.local_q.len();
                println!("{id}▸{:>5}ms  {} → {}   (RTX, в очереди {left})", ms, cut(&zh, 26), cut(&ru, 50));
                deliver(&state, &cdp, vec![(zh, ru)], ms, "local").await;
            }
            Err(e) => {
                eprintln!("[RTX] {e}");
                let mut st = state.lock().await;
                st.in_flight.remove(&zh);
                st.local_q.push(&zh, false);
                drop(st);
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    }
}

type Clouds = Arc<Vec<(Arc<Cloud>, Arc<std::sync::Mutex<Usage>>)>>;

async fn on_search(state: &Shared, cdp: &Arc<Cdp>, clouds: &Clouds, session: &str, id: u64, ru: String) {
    let t = Instant::now();
    let known = state.lock().await.tr.search_cached(&ru);
    let found = match known {
        Some(q) => Some((q, "кэш")),
        None => {
            let active = state.lock().await.active_cloud();
            let claude = active.and_then(|i| match &*clouds[i].0 {
                Cloud::Claude(c) => Some((c, &clouds[i].1)),
                Cloud::Gemini(_) => None,
            });
            let q = match claude {
                Some((c, usage)) => {
                    usage.lock().unwrap().add(1);
                    match c.search(&ru).await {
                        Ok((q, usd)) => {
                            usage.lock().unwrap().spend(usd);
                            Some((q.zh, q.ru))
                        }
                        Err(_) => None,
                    }
                }
                None => None,
            };
            let local = state.lock().await.local;
            let q = match q {
                Some(q) => Some((q, "Claude")),
                None if !local => None,
                None => {
                    let (http, url) = state.lock().await.tr.client();
                    let prompt = format!("把下面的文本翻译成中文，不要额外解释。\n\n{ru}");
                    Translator::model(&http, &url, prompt, &ru).await.ok().map(|(zh, _)| ((zh, ru.clone()), "RTX"))
                }
            };
            if let Some(((zh, meaning), _)) = &q {
                state.lock().await.tr.remember_search(&ru, zh, meaning);
            }
            q
        }
    };
    let (zh, meaning) = match &found {
        Some(((zh, m), src)) => {
            println!("🔍 {:>5}ms  {} → {} ({m}) [{src}]", t.elapsed().as_millis(), cut(&ru, 30), zh);
            (json!(zh), json!(m))
        }
        None => {
            println!("🔍 не получилось перевести «{}» — ищу как написано", cut(&ru, 30));
            (Value::Null, Value::Null)
        }
    };
    cdp.eval(session, format!("window.__tbtr_search_done&&window.__tbtr_search_done({id},{zh},{meaning})"));
}

struct Exit {
    warp_ours: bool,
    llama: std::sync::Mutex<Option<tokio::process::Child>>,
}

async fn shutdown(state: &Shared, exit: &Exit, close_taobao: bool) -> ! {
    {
        let mut st = state.lock().await;
        st.tr.save();
        println!("Кэш сохранён (облако {} / RTX {}).", st.stats.0, st.stats.1);
    }
    if let Some(mut c) = exit.llama.lock().unwrap().take() {
        let _ = c.start_kill();
        println!("Локальная модель остановлена.");
    }
    if close_taobao {
        let _ = std::process::Command::new("taskkill").args(["/F", "/IM", "淘宝桌面版.exe"]).output();
        println!("Taobao закрыт.");
    }
    if exit.warp_ours {
        warp::off();
        println!("WARP выключен.");
    }
    println!("Выход.");
    std::process::exit(0);
}

async fn http_ok(url: &str) -> bool {
    match reqwest::Client::new().get(url).timeout(Duration::from_secs(2)).send().await {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}

async fn ensure_model(root: &Path) -> Result<Option<tokio::process::Child>> {
    let health = format!("http://127.0.0.1:{MODEL_PORT}/health");
    if http_ok(&health).await {
        println!("Локальная модель уже запущена.");
        return Ok(None);
    }
    println!("Запускаю Hunyuan-MT на RTX…");
    let log = std::fs::File::create(root.join("data").join("llama.log"))?;
    let child = tokio::process::Command::new(root.join("bin/llama/llama-server.exe"))
        .args([
            "-m", &root.join("models/Hunyuan-MT-7B.Q4_K_M.gguf").to_string_lossy(),
            "--device", "CUDA0", "-ngl", "26", "-fit", "off", "-c", "4096", "-np", &LOCAL_WORKERS.to_string(), "-t", "8",
            "--host", "127.0.0.1", "--port", &MODEL_PORT.to_string(),
        ])
        .env("CUDA_VISIBLE_DEVICES", "0")
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .kill_on_drop(true)
        .spawn()
        .context("llama-server.exe")?;
    for _ in 0..120 {
        if http_ok(&health).await {
            println!("Локальная модель готова.");
            return Ok(Some(child));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(anyhow!("модель не запустилась, см. data\\llama.log"))
}

fn taobao_exe() -> PathBuf {
    let dir = std::env::var("APPDATA")
        .ok()
        .and_then(|a| std::fs::read_to_string(Path::new(&a).join("taobao/install-location.txt")).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| r"D:\All\taobao".to_string());
    Path::new(&dir).join("淘宝桌面版.exe")
}

async fn ensure_taobao() -> Result<String> {
    let ver = format!("http://127.0.0.1:{CDP_PORT}/json/version");
    if !http_ok(&ver).await {
        println!("Перезапускаю Taobao с портом перевода…");
        let _ = std::process::Command::new("taskkill").args(["/F", "/IM", "淘宝桌面版.exe"]).output();
        tokio::time::sleep(Duration::from_secs(2)).await;
        std::process::Command::new(taobao_exe())
            .arg(format!("--remote-debugging-port={CDP_PORT}"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("не нашёл 淘宝桌面版.exe")?;
        let mut ok = false;
        for _ in 0..60 {
            if http_ok(&ver).await {
                ok = true;
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        if !ok {
            return Err(anyhow!("Taobao не открыл порт {CDP_PORT}"));
        }
    }
    let v: Value = reqwest::get(&ver).await?.json().await?;
    v["webSocketDebuggerUrl"].as_str().map(String::from).ok_or_else(|| anyhow!("нет webSocketDebuggerUrl"))
}

fn find_root() -> PathBuf {
    if let Ok(r) = std::env::var("TBTR_ROOT") {
        return r.into();
    }
    if let Ok(exe) = std::env::current_exe() {
        for a in exe.ancestors() {
            if a.join("data").join("glossary.json").exists() {
                return a.to_path_buf();
            }
        }
    }
    r"D:\All\taobao-translator".into()
}

#[tokio::main]
async fn main() -> Result<()> {
    let _single = match std::net::TcpListener::bind("127.0.0.1:9339") {
        Ok(l) => l,
        Err(_) => {
            println!("Переводчик уже запущен в другом окне.");
            return Ok(());
        }
    };
    let root = find_root();
    let data = root.join("data");
    println!("Taobao → русский. Папка: {}", root.display());

    let tr = Translator::load(&data, &format!("http://127.0.0.1:{MODEL_PORT}/v1/chat/completions"))?;
    println!("Словарь и кэш: {} готовых переводов", tr.cache.len());

    let local = !std::env::args().any(|a| a == "--no-local");
    println!(
        "Режим: {}",
        if local { "облако + RTX (личные страницы переводит RTX)" } else { "лёгкий, без локальной модели (личные страницы — только словарь)" }
    );
    let mut clouds: Vec<Arc<Cloud>> = Vec::new();
    if let Some(k) = cloud::read_key(&data, "claude.key") {
        clouds.push(Arc::new(Cloud::Claude(Claude::new(k)?)));
    }
    if let Some(k) = cloud::read_key(&data, "gemini.key") {
        match Gemini::connect(k).await {
            Ok(g) => clouds.push(Arc::new(Cloud::Gemini(g))),
            Err(e) => println!("Gemini недоступен ({e})."),
        }
    }
    if clouds.is_empty() {
        println!("Облако: нет ключей в data\\claude.key и data\\gemini.key — {}.", if local { "всё переводит RTX" } else { "работает только словарь" });
    }
    for (i, c) in clouds.iter().enumerate() {
        let role = if i == 0 { "основное" } else { "запасное" };
        println!("Облако {role}: {} ({}) — тексты и названия, кроме личных страниц.", c.name(), c.model());
    }

    let warp_ours = if warp::installed() {
        println!("Включаю WARP…");
        let ours = warp::ensure_on().await?;
        println!("WARP {}.", if ours { "включён (выключу при выходе)" } else { "уже был включён (оставлю как есть)" });
        ours
    } else {
        println!("Cloudflare WARP не найден — запускаю без него.");
        false
    };
    let exit = Arc::new(Exit { warp_ours, llama: std::sync::Mutex::new(None) });
    if local {
        *exit.llama.lock().unwrap() = ensure_model(&root).await?;
    }
    let ws_url = ensure_taobao().await?;

    let (ws, _) = tokio_tungstenite::connect_async(&ws_url).await.context("devtools")?;
    let (mut sink, mut stream) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
    tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if sink.send(m).await.is_err() {
                break;
            }
        }
    });
    let cdp = Arc::new(Cdp { tx, next: AtomicU64::new(1), pending: Mutex::new(HashMap::new()) });
    let state: Shared = Arc::new(Mutex::new(State {
        tr,
        subs: HashMap::new(),
        private_sessions: HashSet::new(),
        private_strs: HashSet::new(),
        target_session: HashMap::new(),
        cloud_q: Queue::default(),
        local_q: Queue::default(),
        in_flight: HashSet::new(),
        cloud_off: vec![None; clouds.len()],
        stats: (0, 0),
        local,
    }));
    let wake = Arc::new(Notify::new());
    for i in (1..=LOCAL_WORKERS).filter(|_| local) {
        tokio::spawn(local_worker(state.clone(), cdp.clone(), wake.clone(), i));
    }
    let clouds: Clouds = Arc::new(clouds.into_iter().map(|c| (c.clone(), Arc::new(std::sync::Mutex::new(c.usage(&data))))).collect());
    for (rank, (c, usage)) in clouds.iter().enumerate() {
        println!("☁ {}", usage.lock().unwrap().line(c.name()));
        for _ in 0..c.pace().workers {
            tokio::spawn(cloud_worker(state.clone(), cdp.clone(), wake.clone(), c.clone(), rank, usage.clone()));
        }
    }

    {
        let (state, exit) = (state.clone(), exit.clone());
        tokio::spawn(async move {
            let mut close = tokio::signal::windows::ctrl_close().expect("ctrl_close");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = close.recv() => {}
            }
            shutdown(&state, &exit, true).await;
        });
    }
    {
        let state = state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                state.lock().await.tr.save();
            }
        });
    }

    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<Value>();
    {
        let cdp = cdp.clone();
        let (state, exit) = (state.clone(), exit.clone());
        tokio::spawn(async move {
            while let Some(Ok(m)) = stream.next().await {
                let Message::Text(t) = m else { continue };
                let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
                if let Some(id) = v.get("id").and_then(|x| x.as_u64()) {
                    if let Some(tx) = cdp.pending.lock().await.remove(&id) {
                        let _ = tx.send(v);
                    }
                } else if ev_tx.send(v).is_err() {
                    break;
                }
            }
            println!("Taobao закрыт.");
            shutdown(&state, &exit, false).await;
        });
    }

    cdp.call("Target.setDiscoverTargets", json!({"discover": true}), None).await?;
    let mut attached: HashMap<String, String> = HashMap::new();
    let initial = cdp.call("Target.getTargets", json!({}), None).await?;
    let mut infos: Vec<Value> = initial["targetInfos"].as_array().cloned().unwrap_or_default();

    println!("Перевод включён. Alt+T в окне Taobao — показать оригинал. Закрыть это окно — выключить перевод.");
    loop {
        for info in infos.drain(..) {
            let id = info["targetId"].as_str().unwrap_or("").to_string();
            let url = info["url"].as_str().unwrap_or("").to_string();
            if !wanted(&info) {
                continue;
            }
            match attached.get(&id) {
                None => {
                    attached.insert(id.clone(), url.clone());
                    let (cdp, state) = (cdp.clone(), state.clone());
                    tokio::spawn(async move {
                        match cdp.call("Target.attachToTarget", json!({"targetId": &id, "flatten": true}), None).await {
                            Ok(r) => {
                                if let Some(s) = r["sessionId"].as_str() {
                                    setup_session(cdp.clone(), state, id.clone(), s.to_string(), url).await;
                                }
                            }
                            Err(e) => eprintln!("[!] attach: {e}"),
                        }
                    });
                }
                Some(old) if old != &url => {
                    attached.insert(id.clone(), url.clone());
                    let mut st = state.lock().await;
                    if let Some(s) = st.target_session.get(&id).cloned() {
                        if is_private(&url) {
                            st.private_sessions.insert(s);
                        } else {
                            st.private_sessions.remove(&s);
                        }
                    }
                }
                _ => {}
            }
        }
        let Some(ev) = ev_rx.recv().await else { break };
        match ev["method"].as_str().unwrap_or("") {
            "Target.targetCreated" | "Target.targetInfoChanged" => infos.push(ev["params"]["targetInfo"].clone()),
            "Target.targetDestroyed" | "Target.detachedFromTarget" => {
                if let Some(id) = ev["params"]["targetId"].as_str() {
                    attached.remove(id);
                }
            }
            "Runtime.executionContextCreated" => {
                let ctx = &ev["params"]["context"];
                if let (Some(s), Some(cid)) = (ev["sessionId"].as_str(), ctx["id"].as_i64()) {
                    if ctx["auxData"]["isDefault"] == true {
                        let id = cdp.next.fetch_add(1, Ordering::Relaxed);
                        let msg = json!({"id": id, "method": "Runtime.evaluate", "sessionId": s,
                            "params": {"expression": format!("if(!window.__tbtr){{{INJECT}}}"), "contextId": cid}});
                        let _ = cdp.tx.send(Message::Text(msg.to_string()));
                    }
                }
            }
            "Runtime.bindingCalled" if ev["params"]["name"] == "__tbtr_send" => {
                if let (Some(s), Some(p)) = (ev["sessionId"].as_str(), ev["params"]["payload"].as_str()) {
                    match serde_json::from_str::<Value>(p) {
                        Ok(v) if v["search"].is_string() => {
                            let (state, cdp, clouds, s) = (state.clone(), cdp.clone(), clouds.clone(), s.to_string());
                            let id = v["id"].as_u64().unwrap_or(0);
                            let ru = v["search"].as_str().unwrap_or("").to_string();
                            tokio::spawn(async move { on_search(&state, &cdp, &clouds, &s, id, ru).await });
                        }
                        _ => on_request(&state, &cdp, &wake, s, p).await,
                    }
                }
            }
            _ => {}
        }
    }
    state.lock().await.tr.save();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_text_never_goes_to_the_cloud() {
        assert_eq!(route(true, true, true), Route::Local);
        assert_eq!(route(true, true, false), Route::DraftOnly);
    }

    #[test]
    fn without_local_model_text_waits_for_the_cloud_or_stays_a_draft() {
        assert_eq!(route(false, true, false), Route::Cloud);
        assert_eq!(route(false, false, false), Route::DraftOnly);
        assert_eq!(route(false, false, true), Route::Local);
    }
}
