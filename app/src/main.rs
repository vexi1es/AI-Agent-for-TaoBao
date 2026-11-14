mod cloud;
mod translate;
mod item;
mod money;
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
static SCRIPT: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn script() -> &'static str {
    SCRIPT.get().map(String::as_str).unwrap_or(INJECT)
}

fn page_config(rates: Option<&money::Rates>, s: &money::Settings) -> String {
    let cfg = match rates {
        Some(r) => json!({"uzsPerCny": r.uzs_per_cny * s.markup}),
        None => json!({}),
    };
    format!("window.__tbtr_cfg={cfg};")
}

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
        let _ = cdp.call("Page.addScriptToEvaluateOnNewDocument", json!({"source": script()}), Some(&session)).await;
        cdp.call("Runtime.evaluate", json!({"expression": script()}), Some(&session)).await?;
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

#[derive(Default, Debug, PartialEq)]
struct Asked {
    ready: HashMap<String, String>,
    drafts: HashMap<String, String>,
    queued: bool,
}

enum After {
    Wait(Duration),
    Next,
}

impl State {
    fn new(tr: Translator, clouds: usize, local: bool) -> Self {
        Self {
            tr,
            subs: HashMap::new(),
            private_sessions: HashSet::new(),
            private_strs: HashSet::new(),
            target_session: HashMap::new(),
            cloud_q: Queue::default(),
            local_q: Queue::default(),
            in_flight: HashSet::new(),
            cloud_off: vec![None; clouds],
            stats: (0, 0),
            local,
        }
    }

    fn ask(&mut self, session: &str, v: &Value) -> Asked {
        let private = self.private_sessions.contains(session);
        let way = route(private, self.cloud_available(), self.local);
        let mut out = Asked::default();
        for (list, high) in [("hi", true), ("lo", false)] {
            for s in v[list].as_array().into_iter().flatten().filter_map(|x| x.as_str()) {
                match self.tr.fast(s) {
                    Hit::Fast(t) => {
                        out.ready.insert(s.to_string(), t);
                    }
                    Hit::Miss => out.queued |= self.miss(session, s, private, &way, high, &mut out.drafts),
                }
            }
        }
        out
    }

    fn miss(&mut self, session: &str, s: &str, private: bool, way: &Route, high: bool, drafts: &mut HashMap<String, String>) -> bool {
        if let Some(d) = self.tr.draft(s) {
            drafts.insert(s.to_string(), d);
        }
        self.subs.entry(s.to_string()).or_default().insert(session.to_string());
        if private {
            self.private_strs.insert(s.to_string());
        }
        if self.in_flight.contains(s) {
            return false;
        }
        match way {
            Route::Cloud => self.cloud_q.push(s, high),
            Route::Local => self.local_q.push(s, high),
            Route::DraftOnly => return false,
        }
        true
    }

    fn done(&mut self, results: &[(String, String)], ms: u128, src: &str) -> HashMap<String, HashMap<String, String>> {
        let mut per_session: HashMap<String, HashMap<String, String>> = HashMap::new();
        for (zh, ru) in results {
            self.tr.remember(zh, ru, ms, src);
            self.in_flight.remove(zh);
            for s in self.subs.remove(zh).unwrap_or_default() {
                per_session.entry(s).or_default().insert(zh.clone(), ru.clone());
            }
        }
        if src == "cloud" {
            self.stats.0 += results.len()
        } else {
            self.stats.1 += results.len()
        }
        per_session
    }

    fn steal_local(&mut self, max: usize) {
        for high in [true, false] {
            let from = if high { &mut self.local_q.hi } else { &mut self.local_q.lo };
            let (take, keep): (Vec<String>, Vec<String>) = std::mem::take(from).into_iter().partition(|x| !self.private_strs.contains(x));
            let room = max.saturating_sub(self.cloud_q.len());
            let (now, later) = take.split_at(take.len().min(room));
            let from = if high { &mut self.local_q.hi } else { &mut self.local_q.lo };
            from.extend(later.iter().cloned().chain(keep));
            for x in now {
                self.cloud_q.push(x, high);
            }
        }
    }

    fn cloud_batch(&mut self, rank: usize, size: usize) -> Option<(Vec<String>, Vec<(String, String)>)> {
        if self.active_cloud() != Some(rank) {
            return None;
        }
        if self.cloud_q.len() == 0 {
            self.steal_local(size);
        }
        let b: Vec<String> = self.cloud_q.pick_batch(size).into_iter().filter(|s| matches!(self.tr.fast(s), Hit::Miss)).collect();
        if b.is_empty() {
            return None;
        }
        self.in_flight.extend(b.iter().cloned());
        let terms = self.tr.terms_for(&b);
        Some((b, terms))
    }

    fn cloud_failed(&mut self, rank: usize, batch: Vec<String>, err: &CloudErr, pause: Duration, busy_wait: Duration) -> After {
        match err {
            CloudErr::Busy => {
                self.requeue_front(batch);
                After::Wait(busy_wait)
            }
            CloudErr::Off(_) => {
                self.cloud_off[rank] = Some(Instant::now() + pause);
                self.hand_over(batch, self.cloud_available());
                After::Next
            }
            CloudErr::Other(_) => {
                self.hand_over(batch, false);
                After::Next
            }
        }
    }

    fn requeue_front(&mut self, batch: Vec<String>) {
        for s in batch.into_iter().rev() {
            self.in_flight.remove(&s);
            self.cloud_q.hi.push_front(s);
        }
    }

    fn hand_over(&mut self, batch: Vec<String>, retry_cloud: bool) {
        for s in batch {
            self.in_flight.remove(&s);
            match route(false, retry_cloud, self.local) {
                Route::Cloud => self.cloud_q.push(&s, true),
                Route::Local => self.local_q.push(&s, true),
                Route::DraftOnly => {}
            }
        }
        if !self.cloud_available() && self.local {
            for s in self.cloud_q.pick_batch(usize::MAX) {
                self.local_q.push(&s, false);
            }
        }
    }

    fn next_route_name(&self) -> &'static str {
        match route(false, self.cloud_available(), self.local) {
            Route::Cloud => "запасное облако",
            Route::Local => "RTX",
            Route::DraftOnly => "только словарь",
        }
    }

    fn local_job(&mut self) -> Option<String> {
        while let Some(s) = self.local_q.pick_one() {
            if !self.tr.cache.contains_key(&s) && !self.in_flight.contains(&s) {
                self.in_flight.insert(s.clone());
                return Some(s);
            }
        }
        None
    }

    fn local_failed(&mut self, zh: &str) {
        self.in_flight.remove(zh);
        self.local_q.push(zh, false);
    }
}

async fn on_request(state: &Shared, cdp: &Arc<Cdp>, wake: &Arc<Notify>, session: &str, payload: &str) {
    let Ok(v) = serde_json::from_str::<Value>(payload) else { return };
    let asked = state.lock().await.ask(session, &v);
    cdp.apply(session, &asked.ready);
    cdp.draft(session, &asked.drafts);
    if asked.queued {
        wake.notify_waiters();
    }
}

async fn deliver(state: &Shared, cdp: &Arc<Cdp>, results: Vec<(String, String)>, ms: u128, src: &str) {
    let per_session = state.lock().await.done(&results, ms, src);
    for (s, map) in per_session {
        cdp.apply(&s, &map);
    }
}

async fn idle(wake: &Notify) {
    let _ = tokio::time::timeout(Duration::from_millis(500), wake.notified()).await;
}

fn off_pause(cloud: &Cloud, usage: &std::sync::Mutex<Usage>) -> Duration {
    match cloud {
        Cloud::Gemini(_) => usage.lock().unwrap().until_reset() + Duration::from_secs(120),
        Cloud::Claude(_) => Duration::from_secs(1800),
    }
}

async fn cloud_worker(state: Shared, cdp: Arc<Cdp>, wake: Arc<Notify>, cloud: Arc<Cloud>, rank: usize, usage: Arc<std::sync::Mutex<Usage>>) {
    let pace = cloud.pace();
    let mut last = Instant::now() - pace.gap;
    loop {
        let Some((batch, terms)) = state.lock().await.cloud_batch(rank, pace.batch) else {
            idle(&wake).await;
            continue;
        };
        tokio::time::sleep(pace.gap.saturating_sub(last.elapsed())).await;
        last = Instant::now();
        usage.lock().unwrap().add(batch.len());
        match cloud.translate(&batch, &terms).await {
            Ok(done) => cloud_done(&state, &cdp, &cloud, &usage, batch, done, last.elapsed().as_millis()).await,
            Err(e) => {
                let pause = off_pause(&cloud, &usage);
                let after = state.lock().await.cloud_failed(rank, batch, &e, pause, pace.busy_wait);
                report_failure(&state, &cloud, &e, pause).await;
                wake.notify_waiters();
                if let After::Wait(d) = after {
                    tokio::time::sleep(d).await;
                }
            }
        }
    }
}

async fn cloud_done(state: &Shared, cdp: &Arc<Cdp>, cloud: &Cloud, usage: &std::sync::Mutex<Usage>, batch: Vec<String>, done: cloud::Done, ms: u128) {
    let line = {
        let mut u = usage.lock().unwrap();
        u.spend(done.usd);
        u.line(cloud.name())
    };
    let left = state.lock().await.cloud_q.len();
    println!("☁ {:>5}ms {:>2} строк  {line} (в очереди {left})   напр.: {} → {}", ms, batch.len(), cut(&batch[0], 16), cut(&done.lines[0], 36));
    let res: Vec<(String, String)> = batch.into_iter().zip(done.lines).map(|(z, r)| (z, r.trim().to_string())).collect();
    deliver(state, cdp, res, ms, "cloud").await;
}

async fn report_failure(state: &Shared, cloud: &Cloud, e: &CloudErr, pause: Duration) {
    let st = state.lock().await;
    match e {
        CloudErr::Busy => {}
        CloudErr::Off(msg) => println!("☁ {} выключен на {} мин, дальше переводит {}. Ответ: {}", cloud.name(), pause.as_secs() / 60, st.next_route_name(), cut(msg, 120)),
        CloudErr::Other(err) => println!("☁ {}: ошибка, пачка {}: {}", cloud.name(), if st.local { "уйдёт на RTX" } else { "останется по словарю" }, cut(&err.to_string(), 120)),
    }
}

async fn local_worker(state: Shared, cdp: Arc<Cdp>, wake: Arc<Notify>, id: usize) {
    loop {
        let job = {
            let mut st = state.lock().await;
            st.local_job().map(|s| (st.tr.prompt(&s), st.tr.client(), s))
        };
        let Some((prompt, (http, url), zh)) = job else {
            idle(&wake).await;
            continue;
        };
        match Translator::model(&http, &url, prompt, &zh).await {
            Ok((ru, ms)) => {
                println!("{id}▸{:>5}ms  {} → {}   (RTX)", ms, cut(&zh, 26), cut(&ru, 50));
                deliver(&state, &cdp, vec![(zh, ru)], ms, "local").await;
            }
            Err(e) => {
                eprintln!("[RTX] {e}");
                state.lock().await.local_failed(&zh);
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    }
}

type Clouds = Arc<Vec<(Arc<Cloud>, Arc<std::sync::Mutex<Usage>>)>>;

struct Shop {
    rates: Option<money::Rates>,
    settings: money::Settings,
    seen: std::sync::Mutex<HashMap<String, (usize, Value)>>,
}

fn item_key(url: &str) -> String {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[?&]id=(\d+)").unwrap())
        .captures(url)
        .map_or_else(|| url.split('#').next().unwrap_or(url).to_string(), |c| c[1].to_string())
}

fn claude_of(clouds: &Clouds) -> Option<(&Arc<Cloud>, &Claude, &Arc<std::sync::Mutex<Usage>>)> {
    clouds.iter().find_map(|(c, u)| match &**c {
        Cloud::Claude(cl) => Some((c, cl, u)),
        Cloud::Gemini(_) => None,
    })
}

fn seen_card(shop: &Shop, key: &str, digest: &str) -> Option<Value> {
    let (len, card) = shop.seen.lock().unwrap().get(key).cloned()?;
    (digest.len() * 2 <= len * 3).then_some(card)
}

async fn fix_chinese(cloud: &Cloud, usage: &std::sync::Mutex<Usage>, v: &mut item::Verdict) {
    let left = v.chinese_left();
    if left.is_empty() {
        return;
    }
    usage.lock().unwrap().add(left.len());
    if let Ok(done) = cloud.translate(&left, &[]).await {
        usage.lock().unwrap().spend(done.usd);
        v.replace(&left.into_iter().zip(done.lines).collect::<Vec<_>>());
    }
}

async fn judge(clouds: &Clouds, shop: &Shop, url: &str, text: &str) -> Option<Value> {
    let key = item_key(url);
    let digest = item::digest(text, 6000);
    if let Some(card) = seen_card(shop, &key, &digest) {
        return Some(card);
    }
    let (cloud, c, usage) = claude_of(clouds)?;
    usage.lock().unwrap().add(1);
    let t = Instant::now();
    let (o, usd) = c.ask(item::ITEM_PROMPT, &digest, item::schema()).await.ok()?;
    usage.lock().unwrap().spend(usd);
    let mut v = item::parse_verdict(&o).ok()?;
    fix_chinese(cloud, usage, &mut v).await;
    let card = item::card(&v, shop.rates.as_ref(), &shop.settings);
    println!("🏷 {:>5}ms  {} · {} · {}", t.elapsed().as_millis(), card["label"].as_str().unwrap_or(""), cut(&v.why, 50), card["total"].as_str().unwrap_or(""));
    shop.seen.lock().unwrap().insert(key, (digest.len(), card.clone()));
    Some(card)
}

async fn on_item(cdp: &Arc<Cdp>, clouds: &Clouds, shop: &Shop, session: &str, url: String, text: String) {
    let card = judge(clouds, shop, &url, &text).await.unwrap_or(Value::Null);
    cdp.eval(session, format!("window.__tbtr_item_done&&window.__tbtr_item_done({card})"));
}

type Found = (String, String, &'static str);

async fn search_claude(state: &Shared, clouds: &Clouds, ru: &str) -> Option<Found> {
    let i = state.lock().await.active_cloud()?;
    let (cloud, usage) = &clouds[i];
    let Cloud::Claude(c) = &**cloud else { return None };
    usage.lock().unwrap().add(1);
    let (q, usd) = c.search(ru).await.ok()?;
    usage.lock().unwrap().spend(usd);
    Some((q.zh, q.ru, "Claude"))
}

async fn search_local(state: &Shared, ru: &str) -> Option<Found> {
    let (local, (http, url)) = {
        let st = state.lock().await;
        (st.local, st.tr.client())
    };
    if !local {
        return None;
    }
    let prompt = format!("把下面的文本翻译成中文，不要额外解释。\n\n{ru}");
    let (zh, _) = Translator::model(&http, &url, prompt, ru).await.ok()?;
    Some((zh, ru.to_string(), "RTX"))
}

async fn search_query(state: &Shared, clouds: &Clouds, ru: &str) -> Option<Found> {
    if let Some((zh, m)) = state.lock().await.tr.search_cached(ru) {
        return Some((zh, m, "кэш"));
    }
    let found = match search_claude(state, clouds, ru).await {
        Some(f) => Some(f),
        None => search_local(state, ru).await,
    };
    if let Some((zh, m, _)) = &found {
        state.lock().await.tr.remember_search(ru, zh, m);
    }
    found
}

fn search_reply(id: u64, found: Option<&Found>) -> String {
    let (zh, m) = found.map_or((Value::Null, Value::Null), |(zh, m, _)| (json!(zh), json!(m)));
    format!("window.__tbtr_search_done&&window.__tbtr_search_done({id},{zh},{m})")
}

async fn on_search(state: &Shared, cdp: &Arc<Cdp>, clouds: &Clouds, session: &str, id: u64, ru: String) {
    let t = Instant::now();
    let found = search_query(state, clouds, &ru).await;
    match &found {
        Some((zh, m, src)) => println!("🔍 {:>5}ms  {} → {} ({m}) [{src}]", t.elapsed().as_millis(), cut(&ru, 30), zh),
        None => println!("🔍 не получилось перевести «{}» — ищу как написано", cut(&ru, 30)),
    }
    cdp.eval(session, search_reply(id, found.as_ref()));
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

#[derive(Debug, PartialEq)]
struct Args {
    judge_file: Option<String>,
    local: bool,
}

impl Args {
    fn parse(args: &[String]) -> Self {
        Self {
            judge_file: args.windows(2).find(|w| w[0] == "--judge-file").map(|w| w[1].clone()),
            local: !args.iter().any(|a| a == "--no-local"),
        }
    }
}

#[derive(Debug, PartialEq)]
enum PageMsg {
    Item { url: String, text: String },
    Search { id: u64, ru: String },
    Texts,
}

fn page_msg(payload: &str) -> PageMsg {
    let v: Value = serde_json::from_str(payload).unwrap_or(Value::Null);
    let s = |x: &Value| x.as_str().unwrap_or("").to_string();
    if v["item"].is_object() {
        PageMsg::Item { url: s(&v["item"]["url"]), text: s(&v["item"]["text"]) }
    } else if v["search"].is_string() {
        PageMsg::Search { id: v["id"].as_u64().unwrap_or(0), ru: s(&v["search"]) }
    } else {
        PageMsg::Texts
    }
}

#[derive(Debug, PartialEq)]
enum Event {
    Target(Value),
    Gone(String),
    NewDocument { session: String, context: i64 },
    Page { session: String, payload: String },
    Other,
}

fn event(ev: &Value) -> Event {
    let p = &ev["params"];
    match ev["method"].as_str().unwrap_or("") {
        "Target.targetCreated" | "Target.targetInfoChanged" => Event::Target(p["targetInfo"].clone()),
        "Target.targetDestroyed" | "Target.detachedFromTarget" => p["targetId"].as_str().map_or(Event::Other, |id| Event::Gone(id.into())),
        method => ev["sessionId"].as_str().map_or(Event::Other, |s| session_event(method, s.to_string(), p)),
    }
}

fn session_event(method: &str, session: String, p: &Value) -> Event {
    match method {
        "Runtime.executionContextCreated" if p["context"]["auxData"]["isDefault"] == true => {
            p["context"]["id"].as_i64().map_or(Event::Other, |context| Event::NewDocument { session, context })
        }
        "Runtime.bindingCalled" if p["name"] == "__tbtr_send" => {
            p["payload"].as_str().map_or(Event::Other, |payload| Event::Page { session, payload: payload.into() })
        }
        _ => Event::Other,
    }
}

#[derive(Debug, PartialEq)]
enum TargetChange {
    Attach { id: String, url: String },
    Moved { id: String, private: bool },
    Nothing,
}

#[derive(Default)]
struct Targets {
    attached: HashMap<String, String>,
}

impl Targets {
    fn update(&mut self, info: &Value) -> TargetChange {
        if !wanted(info) {
            return TargetChange::Nothing;
        }
        let id = info["targetId"].as_str().unwrap_or("").to_string();
        let url = info["url"].as_str().unwrap_or("").to_string();
        match self.attached.insert(id.clone(), url.clone()) {
            None => TargetChange::Attach { id, url },
            Some(old) if old != url => TargetChange::Moved { id, private: is_private(&url) },
            Some(_) => TargetChange::Nothing,
        }
    }

    fn gone(&mut self, id: &str) {
        self.attached.remove(id);
    }
}

impl State {
    fn moved(&mut self, target: &str, private: bool) {
        let Some(s) = self.target_session.get(target).cloned() else { return };
        if private {
            self.private_sessions.insert(s);
        } else {
            self.private_sessions.remove(&s);
        }
    }
}

#[derive(Clone)]
struct App {
    state: Shared,
    cdp: Arc<Cdp>,
    clouds: Clouds,
    shop: Arc<Shop>,
    wake: Arc<Notify>,
    exit: Arc<Exit>,
}

impl App {
    fn attach(&self, id: String, url: String) {
        let (cdp, state) = (self.cdp.clone(), self.state.clone());
        tokio::spawn(async move {
            match cdp.call("Target.attachToTarget", json!({"targetId": &id, "flatten": true}), None).await {
                Ok(r) => {
                    if let Some(s) = r["sessionId"].as_str() {
                        setup_session(cdp.clone(), state, id, s.to_string(), url).await;
                    }
                }
                Err(e) => eprintln!("[!] attach: {e}"),
            }
        });
    }

    async fn target(&self, targets: &mut Targets, info: &Value) {
        match targets.update(info) {
            TargetChange::Attach { id, url } => self.attach(id, url),
            TargetChange::Moved { id, private } => self.state.lock().await.moved(&id, private),
            TargetChange::Nothing => {}
        }
    }

    fn reinject(&self, session: &str, context: i64) {
        let id = self.cdp.next.fetch_add(1, Ordering::Relaxed);
        let msg = json!({"id": id, "method": "Runtime.evaluate", "sessionId": session,
            "params": {"expression": format!("if(!window.__tbtr){{{}}}", script()), "contextId": context}});
        let _ = self.cdp.tx.send(Message::Text(msg.to_string()));
    }

    async fn page(&self, session: String, payload: String) {
        let app = self.clone();
        match page_msg(&payload) {
            PageMsg::Item { url, text } => {
                tokio::spawn(async move { on_item(&app.cdp, &app.clouds, &app.shop, &session, url, text).await });
            }
            PageMsg::Search { id, ru } => {
                tokio::spawn(async move { on_search(&app.state, &app.cdp, &app.clouds, &session, id, ru).await });
            }
            PageMsg::Texts => on_request(&self.state, &self.cdp, &self.wake, &session, &payload).await,
        }
    }

    async fn handle(&self, targets: &mut Targets, ev: &Value) {
        match event(ev) {
            Event::Target(info) => self.target(targets, &info).await,
            Event::Gone(id) => targets.gone(&id),
            Event::NewDocument { session, context } => self.reinject(&session, context),
            Event::Page { session, payload } => self.page(session, payload).await,
            Event::Other => {}
        }
    }

    fn start_workers(&self, local: bool) {
        for i in (1..=LOCAL_WORKERS).filter(|_| local) {
            tokio::spawn(local_worker(self.state.clone(), self.cdp.clone(), self.wake.clone(), i));
        }
        for (rank, (c, usage)) in self.clouds.iter().enumerate() {
            println!("☁ {}", usage.lock().unwrap().line(c.name()));
            for _ in 0..c.pace().workers {
                tokio::spawn(cloud_worker(self.state.clone(), self.cdp.clone(), self.wake.clone(), c.clone(), rank, usage.clone()));
            }
        }
    }

    fn watch_exit(&self) {
        let (state, exit) = (self.state.clone(), self.exit.clone());
        tokio::spawn(async move {
            let mut close = tokio::signal::windows::ctrl_close().expect("ctrl_close");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = close.recv() => {}
            }
            shutdown(&state, &exit, true).await;
        });
        let state = self.state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                state.lock().await.tr.save();
            }
        });
    }

    async fn run(&self, mut events: mpsc::UnboundedReceiver<Value>) -> Result<()> {
        self.cdp.call("Target.setDiscoverTargets", json!({"discover": true}), None).await?;
        let initial = self.cdp.call("Target.getTargets", json!({}), None).await?;
        let mut targets = Targets::default();
        for info in initial["targetInfos"].as_array().into_iter().flatten() {
            self.target(&mut targets, info).await;
        }
        println!("Перевод включён. Alt+T в окне Taobao — показать оригинал. Закрыть это окно — выключить перевод.");
        while let Some(ev) = events.recv().await {
            self.handle(&mut targets, &ev).await;
        }
        self.state.lock().await.tr.save();
        Ok(())
    }
}

async fn read_devtools(mut stream: futures_util::stream::SplitStream<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>, cdp: Arc<Cdp>, events: mpsc::UnboundedSender<Value>) {
    while let Some(Ok(m)) = stream.next().await {
        let Message::Text(t) = m else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
        match v.get("id").and_then(|x| x.as_u64()) {
            Some(id) => {
                if let Some(tx) = cdp.pending.lock().await.remove(&id) {
                    let _ = tx.send(v);
                }
            }
            None if events.send(v).is_err() => break,
            None => {}
        }
    }
}

async fn connect_devtools(ws_url: &str) -> Result<(Arc<Cdp>, mpsc::UnboundedReceiver<Value>, impl std::future::Future<Output = ()>)> {
    let (ws, _) = tokio_tungstenite::connect_async(ws_url).await.context("devtools")?;
    let (mut sink, stream) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
    tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if sink.send(m).await.is_err() {
                break;
            }
        }
    });
    let cdp = Arc::new(Cdp { tx, next: AtomicU64::new(1), pending: Mutex::new(HashMap::new()) });
    let (ev_tx, ev_rx) = mpsc::unbounded_channel::<Value>();
    let reader = read_devtools(stream, cdp.clone(), ev_tx);
    Ok((cdp, ev_rx, reader))
}

async fn load_clouds(data: &Path, local: bool) -> Result<Clouds> {
    let mut clouds: Vec<Arc<Cloud>> = Vec::new();
    if let Some(k) = cloud::read_key(data, "claude.key") {
        clouds.push(Arc::new(Cloud::Claude(Claude::new(k)?)));
    }
    if let Some(k) = cloud::read_key(data, "gemini.key") {
        match Gemini::connect(k).await {
            Ok(g) => clouds.push(Arc::new(Cloud::Gemini(g))),
            Err(e) => println!("Gemini недоступен ({e})."),
        }
    }
    println!("{}", clouds_line(&clouds.iter().map(|c| (c.name(), c.model())).collect::<Vec<_>>(), local));
    Ok(Arc::new(clouds.into_iter().map(|c| (c.clone(), Arc::new(std::sync::Mutex::new(c.usage(data))))).collect()))
}

fn clouds_line(clouds: &[(&str, &str)], local: bool) -> String {
    if clouds.is_empty() {
        let rest = if local { "всё переводит RTX" } else { "работает только словарь" };
        return format!("Облако: нет ключей в data\\claude.key и data\\gemini.key — {rest}.");
    }
    let names: Vec<String> = clouds
        .iter()
        .enumerate()
        .map(|(i, (n, m))| format!("{} {n} ({m})", if i == 0 { "основное" } else { "запасное" }))
        .collect();
    format!("Облако: {} — тексты и названия, кроме личных страниц.", names.join(", "))
}

async fn load_shop(data: &Path) -> Result<Arc<Shop>> {
    let settings = money::Settings::load(data);
    let http = reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?;
    let rates = money::Rates::load(data, &http).await;
    println!("{}", rates_line(rates.as_ref(), &settings));
    let _ = SCRIPT.set(page_config(rates.as_ref(), &settings) + INJECT);
    Ok(Arc::new(Shop { rates, settings, seen: std::sync::Mutex::new(HashMap::new()) }))
}

fn rates_line(r: Option<&money::Rates>, s: &money::Settings) -> String {
    match r {
        Some(r) => format!(
            "Курс ЦБ на {}: 1 ¥ = {:.2} сум, 1 $ = {:.2} сум; наценка ×{}, карго {} ${}/кг.",
            r.date, r.uzs_per_cny, r.uzs_per_usd, s.markup, s.cargo_name, s.cargo_usd_per_kg
        ),
        None => "Курс ЦБ недоступен — цены в сумах не показываю.".into(),
    }
}

async fn start_outside(root: &Path, local: bool) -> Result<Arc<Exit>> {
    let installed = warp::installed();
    let warp_ours = if installed { warp::ensure_on().await? } else { false };
    println!("{}", warp_line(installed, warp_ours));
    let llama = if local { ensure_model(root).await? } else { None };
    Ok(Arc::new(Exit { warp_ours, llama: std::sync::Mutex::new(llama) }))
}

fn warp_line(installed: bool, ours: bool) -> &'static str {
    match (installed, ours) {
        (false, _) => "Cloudflare WARP не найден — запускаю без него.",
        (true, true) => "WARP включён (выключу при выходе).",
        (true, false) => "WARP уже был включён (оставлю как есть).",
    }
}

async fn judge_and_print(clouds: &Clouds, shop: &Shop, f: &str) -> Result<()> {
    let text = std::fs::read_to_string(f).with_context(|| f.to_string())?;
    let card = judge(clouds, shop, f, &text).await;
    println!("{}", serde_json::to_string_pretty(&card).unwrap_or_default());
    Ok(())
}

fn mode_line(local: bool) -> &'static str {
    match local {
        true => "Режим: облако + RTX (личные страницы переводит RTX)",
        false => "Режим: лёгкий, без локальной модели (личные страницы — только словарь)",
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse(&std::env::args().collect::<Vec<_>>());
    let single = std::net::TcpListener::bind("127.0.0.1:9339");
    if args.judge_file.is_none() && single.is_err() {
        println!("Переводчик уже запущен в другом окне.");
        return Ok(());
    }
    let root = find_root();
    let data = root.join("data");
    let tr = Translator::load(&data, &format!("http://127.0.0.1:{MODEL_PORT}/v1/chat/completions"))?;
    println!("Taobao → русский. Папка: {}. Словарь и кэш: {} готовых переводов.", root.display(), tr.cache.len());
    println!("{}", mode_line(args.local));
    let clouds = load_clouds(&data, args.local).await?;
    let shop = load_shop(&data).await?;
    if let Some(f) = &args.judge_file {
        return judge_and_print(&clouds, &shop, f).await;
    }
    let exit = start_outside(&root, args.local).await?;
    let (cdp, events, reader) = connect_devtools(&ensure_taobao().await?).await?;
    let state: Shared = Arc::new(Mutex::new(State::new(tr, clouds.len(), args.local)));
    let app = App { state, cdp, clouds, shop, wake: Arc::new(Notify::new()), exit };
    app.start_workers(args.local);
    app.watch_exit();
    let closing = app.clone();
    tokio::spawn(async move {
        reader.await;
        println!("Taobao закрыт.");
        shutdown(&closing.state, &closing.exit, false).await;
    });
    app.run(events).await
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

    fn state(clouds: usize, local: bool) -> State {
        let dir = std::env::temp_dir().join(format!("tbtr_st_{}_{:?}", std::process::id(), std::thread::current().id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("glossary.json"), r#"{"宝贝": "товар", "男士": "мужские", "新款": "новинка"}"#).unwrap();
        let _ = std::fs::remove_file(dir.join("cache.json"));
        State::new(Translator::load(&dir, "http://127.0.0.1:1").unwrap(), clouds, local)
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn queue_keeps_visible_text_first_and_never_twice() {
        let mut q = Queue::default();
        q.push("长长长长", true);
        q.push("短", true);
        q.push("下面", false);
        q.push("短", false);
        q.push("下面", false);
        assert_eq!(q.len(), 3);
        assert_eq!(q.pick_one().as_deref(), Some("短"), "shortest visible first");
        assert_eq!(q.pick_batch(5), strings(&["长长长长", "下面"]));
        assert_eq!(q.pick_one(), None);
        for s in ["一", "二", "三"] {
            q.push(s, false);
        }
        assert_eq!(q.pick_batch(2), strings(&["一", "二"]), "a batch is never bigger than asked");
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn known_text_is_answered_at_once_and_new_text_is_queued_for_the_cloud() {
        let mut st = state(1, true);
        let asked = st.ask("s1", &json!({"hi": ["宝贝", "男士新款外套"], "lo": ["别的"]}));
        assert_eq!(asked.ready.get("宝贝").map(String::as_str), Some("товар"));
        assert_eq!(asked.drafts.get("男士新款外套").map(String::as_str), Some("мужские · новинка …"));
        assert!(asked.queued);
        assert_eq!(st.cloud_q.pick_batch(9), strings(&["男士新款外套", "别的"]));
        assert_eq!(st.local_q.len(), 0);
    }

    #[test]
    fn private_pages_go_to_the_local_model_or_stay_drafts() {
        let mut st = state(1, true);
        st.private_sessions.insert("p".into());
        st.ask("p", &json!({"hi": ["收货地址"]}));
        assert_eq!((st.cloud_q.len(), st.local_q.len()), (0, 1));
        assert!(st.private_strs.contains("收货地址"));
        let mut light = state(1, false);
        light.private_sessions.insert("p".into());
        assert!(!light.ask("p", &json!({"hi": ["收货地址"]})).queued);
        assert_eq!((light.cloud_q.len(), light.local_q.len()), (0, 0));
    }

    #[test]
    fn text_already_being_translated_is_not_queued_again() {
        let mut st = state(1, true);
        st.in_flight.insert("别的".into());
        assert!(!st.ask("s", &json!({"hi": ["别的"]})).queued);
        assert_eq!(st.subs["别的"].len(), 1, "the page still gets the answer");
    }

    #[test]
    fn results_go_to_every_page_that_asked_and_are_remembered() {
        let mut st = state(1, true);
        st.ask("a", &json!({"hi": ["别的"]}));
        st.ask("b", &json!({"lo": ["别的"]}));
        st.in_flight.insert("别的".into());
        let per = st.done(&[("别的".into(), "Другое".into())], 5, "cloud");
        assert_eq!(per.len(), 2);
        assert_eq!(per["a"]["别的"], "Другое");
        assert!(st.in_flight.is_empty());
        assert_eq!(st.stats, (1, 0));
        st.done(&[("一".into(), "1".into()), ("二".into(), "2".into())], 5, "cloud");
        st.done(&[("三".into(), "3".into()), ("四".into(), "4".into())], 5, "local");
        assert_eq!(st.stats, (3, 2), "cloud and local counts add up");
        assert!(matches!(st.tr.fast("别的"), Hit::Fast(t) if t == "Другое"));
    }

    #[test]
    fn idle_cloud_takes_waiting_local_work_but_not_private_text() {
        let mut st = state(1, true);
        st.local_q.push("一", true);
        st.local_q.push("私", true);
        st.local_q.push("二", false);
        st.private_strs.insert("私".into());
        let (b, _) = st.cloud_batch(0, 10).unwrap();
        assert_eq!(b, strings(&["一", "二"]));
        assert_eq!(st.local_q.pick_batch(9), strings(&["私"]));
        assert!(st.in_flight.contains("一"));
    }

    #[test]
    fn only_the_first_working_cloud_takes_batches() {
        let mut st = state(2, true);
        st.cloud_q.push("一", true);
        assert!(st.cloud_batch(1, 10).is_none(), "reserve waits while the main cloud works");
        st.cloud_off[0] = Some(Instant::now() + Duration::from_secs(60));
        assert!(st.cloud_batch(1, 10).is_some());
    }

    #[test]
    fn busy_cloud_puts_the_batch_back_in_front() {
        let mut st = state(1, true);
        st.cloud_q.push("三", true);
        let after = st.cloud_failed(0, strings(&["一", "二"]), &CloudErr::Busy, Duration::ZERO, Duration::from_secs(2));
        assert!(matches!(after, After::Wait(d) if d == Duration::from_secs(2)));
        assert_eq!(st.cloud_q.pick_batch(9), strings(&["一", "二", "三"]));
    }

    #[test]
    fn switched_off_cloud_hands_work_to_the_reserve_then_to_the_local_model() {
        let mut st = state(2, true);
        let off = CloudErr::Off("no credit".into());
        st.cloud_failed(0, strings(&["一"]), &off, Duration::from_secs(60), Duration::ZERO);
        assert_eq!(st.active_cloud(), Some(1));
        assert_eq!(st.next_route_name(), "запасное облако");
        assert_eq!(st.cloud_q.pick_batch(9), strings(&["一"]));
        st.cloud_q.push("二", false);
        st.cloud_failed(1, strings(&["一"]), &off, Duration::from_secs(60), Duration::ZERO);
        assert_eq!(st.next_route_name(), "RTX");
        assert_eq!(st.local_q.pick_batch(9), strings(&["一", "二"]));
    }

    #[test]
    fn a_broken_batch_is_not_sent_to_the_same_cloud_again() {
        let mut st = state(1, false);
        st.cloud_failed(0, strings(&["一"]), &CloudErr::Other(anyhow!("bad JSON")), Duration::ZERO, Duration::ZERO);
        assert_eq!((st.cloud_q.len(), st.local_q.len()), (0, 0));
        assert_eq!(st.active_cloud(), Some(0), "one broken answer does not switch the cloud off");
        let mut rtx = state(1, true);
        rtx.cloud_failed(0, strings(&["一"]), &CloudErr::Other(anyhow!("bad JSON")), Duration::ZERO, Duration::ZERO);
        assert_eq!(rtx.local_q.len(), 1);
    }

    #[test]
    fn local_jobs_skip_known_and_running_text_and_come_back_after_a_failure() {
        let mut st = state(0, true);
        st.local_q.push("宝贝", true);
        st.local_q.push("一", true);
        st.local_q.push("二", true);
        st.tr.cache.insert("宝贝".into(), "товар".into());
        st.in_flight.insert("一".into());
        assert_eq!(st.local_job().as_deref(), Some("二"));
        assert_eq!(st.local_job(), None);
        st.local_failed("二");
        assert_eq!(st.local_job().as_deref(), Some("二"));
    }

    #[test]
    fn only_real_pages_are_watched_and_personal_ones_stay_local() {
        assert!(wanted(&json!({"type": "page", "url": "https://s.taobao.com/search?q=1"})));
        assert!(wanted(&json!({"type": "iframe", "url": "https://x.taobao.com"})));
        for (t, u) in [("page", "devtools://x"), ("page", "chrome://gpu"), ("page", ""), ("page", "about:blank"), ("service_worker", "https://www.taobao.com/sw.js")] {
            assert!(!wanted(&json!({"type": t, "url": u})), "{t} {u}");
        }
        for u in ["https://buyertrade.taobao.com/trade/itemlist", "https://market.m.taobao.com/app/im/chat-core/index.html", "https://login.taobao.com/", "https://member1.taobao.com/member/fresh/deliver_address.htm", "file:///D:/taobao/pages/purchaseListBubble/index.html"] {
            assert!(is_private(u), "{u}");
        }
        for u in ["https://s.taobao.com/search?q=1", "https://item.taobao.com/item.htm?id=1", "https://cart.taobao.com/cart.htm"] {
            assert!(!is_private(u), "{u}");
        }
    }

    #[test]
    fn product_key_is_the_item_id() {
        assert_eq!(item_key("https://item.taobao.com/item.htm?spm=a&id=611830625730&ns=1"), "611830625730");
        assert_eq!(item_key("https://detail.tmall.com/item.htm?id=42#reviews"), "42");
        assert_eq!(item_key("bench/item_boots.txt#x"), "bench/item_boots.txt");
    }

    #[test]
    fn pages_get_the_real_page_script() {
        assert!(script().contains("window.__tbtr = {"));
        assert!(script().contains("__tbtr_item_done"));
    }

    #[test]
    fn pages_get_the_soum_rate_with_the_markup_or_nothing() {
        let r = money::Rates { uzs_per_cny: 1800.0, uzs_per_usd: 12000.0, date: "x".into() };
        let s = money::Settings { markup: 1.5, ..money::Settings::default() };
        assert_eq!(page_config(Some(&r), &s), r#"window.__tbtr_cfg={"uzsPerCny":2700.0};"#);
        assert_eq!(page_config(None, &s), "window.__tbtr_cfg={};");
    }

    #[test]
    fn switched_off_claude_waits_half_an_hour_and_gemini_until_its_quota_resets() {
        let dir = std::env::temp_dir().join(format!("tbtr_op_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let c = Cloud::Claude(Claude::new("k".into()).unwrap());
        let u = std::sync::Mutex::new(c.usage(&dir));
        assert_eq!(off_pause(&c, &u), Duration::from_secs(1800));
    }

    fn args(v: &[&str]) -> Args {
        Args::parse(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn command_line_picks_light_mode_and_the_judge_file() {
        assert_eq!(args(&["tr.exe"]), Args { judge_file: None, local: true });
        assert_eq!(args(&["tr.exe", "--no-local"]), Args { judge_file: None, local: false });
        assert_eq!(args(&["tr.exe", "--judge-file", "a.txt", "--no-local"]), Args { judge_file: Some("a.txt".into()), local: false });
        assert_eq!(args(&["tr.exe", "--judge-file"]).judge_file, None);
    }

    #[test]
    fn page_messages_are_items_searches_or_texts() {
        assert_eq!(page_msg(r#"{"item":{"url":"u","text":"t"}}"#), PageMsg::Item { url: "u".into(), text: "t".into() });
        assert_eq!(page_msg(r#"{"search":"челси","id":3}"#), PageMsg::Search { id: 3, ru: "челси".into() });
        assert_eq!(page_msg(r#"{"hi":["宝贝"],"lo":[]}"#), PageMsg::Texts);
        assert_eq!(page_msg("not json"), PageMsg::Texts);
    }

    #[test]
    fn devtools_events_are_sorted_out() {
        let info = json!({"targetId": "T", "type": "page", "url": "u"});
        assert_eq!(event(&json!({"method": "Target.targetCreated", "params": {"targetInfo": info}})), Event::Target(info.clone()));
        assert_eq!(event(&json!({"method": "Target.targetInfoChanged", "params": {"targetInfo": info}})), Event::Target(info));
        assert_eq!(event(&json!({"method": "Target.targetDestroyed", "params": {"targetId": "T"}})), Event::Gone("T".into()));
        assert_eq!(event(&json!({"method": "Target.detachedFromTarget", "params": {}})), Event::Other);
        let ctx = |default: bool| json!({"method": "Runtime.executionContextCreated", "sessionId": "S", "params": {"context": {"id": 7, "auxData": {"isDefault": default}}}});
        assert_eq!(event(&ctx(true)), Event::NewDocument { session: "S".into(), context: 7 });
        assert_eq!(event(&ctx(false)), Event::Other);
        let bind = |name: &str| json!({"method": "Runtime.bindingCalled", "sessionId": "S", "params": {"name": name, "payload": "{}"}});
        assert_eq!(event(&bind("__tbtr_send")), Event::Page { session: "S".into(), payload: "{}".into() });
        assert_eq!(event(&bind("other")), Event::Other);
        assert_eq!(event(&json!({"method": "Runtime.consoleAPICalled"})), Event::Other);
    }

    #[test]
    fn tabs_are_attached_once_and_rechecked_when_they_navigate() {
        let mut t = Targets::default();
        let info = |url: &str| json!({"targetId": "T", "type": "page", "url": url});
        assert_eq!(t.update(&info("https://s.taobao.com/search")), TargetChange::Attach { id: "T".into(), url: "https://s.taobao.com/search".into() });
        assert_eq!(t.update(&info("https://s.taobao.com/search")), TargetChange::Nothing);
        assert_eq!(t.update(&info("https://buyertrade.taobao.com/trade")), TargetChange::Moved { id: "T".into(), private: true });
        assert_eq!(t.update(&json!({"targetId": "W", "type": "service_worker", "url": "x"})), TargetChange::Nothing);
        t.gone("T");
        assert!(matches!(t.update(&info("https://s.taobao.com/search")), TargetChange::Attach { .. }));
    }

    #[test]
    fn a_tab_that_moves_to_a_personal_page_becomes_local_only_and_back() {
        let mut st = state(1, true);
        st.target_session.insert("T".into(), "S".into());
        st.moved("T", true);
        assert!(st.private_sessions.contains("S"));
        st.moved("T", false);
        assert!(!st.private_sessions.contains("S"));
        st.moved("unknown", true);
        assert_eq!(st.private_sessions.len(), 0);
    }

    #[test]
    fn start_lines_say_what_is_on() {
        assert_eq!(clouds_line(&[], true), "Облако: нет ключей в data\\claude.key и data\\gemini.key — всё переводит RTX.");
        assert!(clouds_line(&[], false).ends_with("работает только словарь."));
        assert_eq!(
            clouds_line(&[("Claude", "h"), ("Gemini", "g")], true),
            "Облако: основное Claude (h), запасное Gemini (g) — тексты и названия, кроме личных страниц."
        );
        let r = money::Rates { uzs_per_cny: 1795.58, uzs_per_usd: 11973.44, date: "13.11.2026".into() };
        assert_eq!(
            rates_line(Some(&r), &money::Settings::default()),
            "Курс ЦБ на 13.11.2026: 1 ¥ = 1795.58 сум, 1 $ = 11973.44 сум; наценка ×1, карго Top Cargo авиа $11/кг."
        );
        assert!(rates_line(None, &money::Settings::default()).contains("недоступен"));
        assert!(warp_line(false, false).contains("не найден"));
        assert!(warp_line(true, true).contains("выключу при выходе"));
        assert!(warp_line(true, false).contains("оставлю как есть"));
        assert!(mode_line(true).contains("RTX"));
        assert!(mode_line(false).contains("лёгкий"));
    }

    #[test]
    fn the_page_gets_the_chinese_query_or_null_to_search_as_typed() {
        let f: Found = ("切尔西靴男".into(), "челси мужские".into(), "кэш");
        assert_eq!(search_reply(4, Some(&f)), r#"window.__tbtr_search_done&&window.__tbtr_search_done(4,"切尔西靴男","челси мужские")"#);
        assert_eq!(search_reply(5, None), "window.__tbtr_search_done&&window.__tbtr_search_done(5,null,null)");
    }

    #[test]
    fn a_product_is_judged_again_only_when_much_more_of_its_page_loaded() {
        let shop = Shop { rates: None, settings: money::Settings::default(), seen: std::sync::Mutex::new(HashMap::new()) };
        shop.seen.lock().unwrap().insert("1".into(), (100, json!({"label": "x"})));
        assert_eq!(seen_card(&shop, "1", &"a".repeat(150)), Some(json!({"label": "x"})));
        assert_eq!(seen_card(&shop, "1", &"a".repeat(151)), None);
        assert_eq!(seen_card(&shop, "2", "a"), None);
    }

    #[test]
    fn product_verdicts_use_claude_only() {
        let dir = std::env::temp_dir().join(format!("tbtr_co_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let c = Arc::new(Cloud::Claude(Claude::new("k".into()).unwrap()));
        let clouds: Clouds = Arc::new(vec![(c.clone(), Arc::new(std::sync::Mutex::new(c.usage(&dir))))]);
        assert!(claude_of(&clouds).is_some());
        let none: Clouds = Arc::new(vec![]);
        assert!(claude_of(&none).is_none());
    }

    #[test]
    fn short_and_cut_keep_log_lines_short() {
        assert_eq!(short("https://a.b/c?x=1"), "https://a.b/c");
        assert_eq!(cut("абвгд", 3), "абв…");
        assert_eq!(cut("аб", 3), "аб");
    }
}
