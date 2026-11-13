use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const PROMPT: &str = include_str!("prompt.txt");

pub enum CloudErr {
    Busy,
    Off(String),
    Other(anyhow::Error),
}

pub struct Pace {
    pub batch: usize,
    pub gap: Duration,
    pub workers: usize,
    pub busy_wait: Duration,
}

pub struct Done {
    pub lines: Vec<String>,
    pub usd: f64,
}

pub enum Cloud {
    Claude(Claude),
    Gemini(Gemini),
}

impl Cloud {
    pub fn name(&self) -> &'static str {
        match self {
            Cloud::Claude(_) => "Claude",
            Cloud::Gemini(_) => "Gemini",
        }
    }

    pub fn model(&self) -> &str {
        match self {
            Cloud::Claude(c) => c.model,
            Cloud::Gemini(g) => &g.model,
        }
    }

    pub fn pace(&self) -> Pace {
        match self {
            Cloud::Claude(_) => Pace { batch: 10, gap: Duration::ZERO, workers: 4, busy_wait: Duration::from_secs(2) },
            Cloud::Gemini(_) => Pace { batch: 80, gap: Duration::from_millis(4300), workers: 1, busy_wait: Duration::from_secs(10) },
        }
    }

    pub fn usage(&self, data: &Path) -> Usage {
        match self {
            Cloud::Claude(_) => Usage::load(data.join("claude_usage.json"), TASHKENT, None),
            Cloud::Gemini(_) => Usage::load(data.join("cloud_usage.json"), PACIFIC, Some(500)),
        }
    }

    pub async fn translate(&self, items: &[String], terms: &[(String, String)]) -> std::result::Result<Done, CloudErr> {
        let glossary = terms.iter().map(|(z, r)| format!("{z} = {r}")).collect::<Vec<_>>().join("\n");
        let system = format!("{PROMPT}{glossary}");
        match self {
            Cloud::Claude(c) => c.translate(&system, items).await,
            Cloud::Gemini(g) => g.translate(&system, items).await.map(|lines| Done { lines, usd: 0.0 }),
        }
    }
}

async fn post_json(req: reqwest::RequestBuilder) -> std::result::Result<(u16, Value), CloudErr> {
    let r = req.send().await.map_err(|e| CloudErr::Other(e.into()))?;
    let status = r.status().as_u16();
    let v: Value = r.json().await.map_err(|e| CloudErr::Other(e.into()))?;
    Ok((status, v))
}

pub fn read_key(data: &Path, file: &str) -> Option<String> {
    let s = std::fs::read_to_string(data.join(file)).ok()?;
    s.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#') && *l != "KEY").map(String::from)
}

const ANTHROPIC: &str = "https://api.anthropic.com/v1/messages";

pub struct Claude {
    http: reqwest::Client,
    key: String,
    pub model: &'static str,
}

fn price(model: &str) -> (f64, f64) {
    if model.contains("opus") {
        (4.0, 20.0)
    } else if model.contains("sonnet") {
        (2.0, 10.0)
    } else {
        (0.10, 0.50)
    }
}

pub fn strings_schema(keys: &[String]) -> Value {
    let props: serde_json::Map<String, Value> = keys.iter().map(|k| (k.clone(), json!({"type": "string"}))).collect();
    json!({"type": "object", "properties": props, "required": keys, "additionalProperties": false})
}

fn claude_body(model: &str, system: &str, user: &str, schema: Value) -> Value {
    json!({
        "model": model,
        "max_tokens": 8000,
        "system": system,
        "output_config": {
            "effort": "low",
            "format": {"type": "json_schema", "schema": schema}
        },
        "messages": [{"role": "user", "content": user}]
    })
}

fn line_keys(n: usize) -> Vec<String> {
    (1..=n).map(|i| i.to_string()).collect()
}

fn numbered(items: &[String]) -> String {
    let m: serde_json::Map<String, Value> = line_keys(items.len()).into_iter().zip(items).map(|(k, s)| (k, json!(s))).collect();
    Value::Object(m).to_string()
}

pub const SEARCH_PROMPT: &str = "Ты помогаешь покупателю из СНГ искать товары на Taobao. \
Преврати его запрос на русском в короткий поисковый запрос на китайском, как его набрал бы китайский покупатель: \
2–6 ключевых слов через пробел, самые важные первыми. Цену, бюджет и вежливые слова убери. \
Бренды, модели, размеры и латиницу оставь как есть. \
Ответ: JSON {\"q\": китайский запрос, \"ru\": очень короткий обратный перевод этого запроса на русский}.";

const MONEY_WORDS: [&str; 4] = ["credit", "billing", "spend", "limit"];

fn about_money(msg: &str) -> bool {
    let low = msg.to_lowercase();
    MONEY_WORDS.iter().any(|w| low.contains(w))
}

fn or_default(msg: String, default: &str) -> String {
    if msg.is_empty() { default.into() } else { msg }
}

fn claude_error(status: u16, v: &Value) -> CloudErr {
    let msg = v["error"]["message"].as_str().unwrap_or("").to_string();
    match status {
        429 | 500 | 502 | 503 | 504 | 529 => CloudErr::Busy,
        401 | 403 => CloudErr::Off(or_default(msg, "ключ не принят")),
        400 if about_money(&msg) => CloudErr::Off(msg),
        _ => CloudErr::Other(anyhow!("HTTP {status}: {msg}")),
    }
}

fn claude_parse(model: &str, v: &Value, n: usize) -> Result<Done> {
    let (o, usd) = claude_object(model, v)?;
    let lines = (1..=n)
        .map(|i| o[i.to_string()].as_str().map(String::from).ok_or_else(|| anyhow!("нет строки {i}")))
        .collect::<Result<Vec<_>>>()?;
    Ok(Done { lines, usd })
}

fn claude_object(model: &str, v: &Value) -> Result<(Value, f64)> {
    let stop = v["stop_reason"].as_str().unwrap_or("");
    if stop != "end_turn" {
        return Err(anyhow!("ответ оборван: {stop}"));
    }
    let text = v["content"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|b| b["type"] == "text")
        .and_then(|b| b["text"].as_str())
        .ok_or_else(|| anyhow!("нет текста в ответе"))?;
    let o: Value = serde_json::from_str(text).map_err(|e| anyhow!("bad JSON: {e}"))?;
    let (pi, po) = price(model);
    let u = &v["usage"];
    let tok = |k: &str| u[k].as_f64().unwrap_or(0.0);
    let usd = (tok("input_tokens") * pi
        + tok("cache_creation_input_tokens") * pi * 1.25
        + tok("cache_read_input_tokens") * pi * 0.1
        + tok("output_tokens") * po)
        / 1e6;
    Ok((o, usd))
}

fn search_parse(o: &Value) -> Result<SearchQuery> {
    let q = o["q"].as_str().map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("пустой запрос"))?;
    Ok(SearchQuery { zh: q.to_string(), ru: o["ru"].as_str().unwrap_or("").trim().to_string() })
}

pub struct SearchQuery {
    pub zh: String,
    pub ru: String,
}

impl Claude {
    pub fn new(key: String) -> Result<Self> {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(90)).build()?;
        Ok(Self { http, key, model: "claude-haiku-5-5" })
    }

    async fn call(&self, body: &Value) -> std::result::Result<Value, CloudErr> {
        let req = self.http.post(ANTHROPIC).header("x-api-key", &self.key).header("anthropic-version", "2026-10-21").json(body);
        let (status, v) = post_json(req).await?;
        if status >= 400 {
            return Err(claude_error(status, &v));
        }
        Ok(v)
    }

    async fn translate(&self, system: &str, items: &[String]) -> std::result::Result<Done, CloudErr> {
        let v = self.call(&claude_body(self.model, system, &numbered(items), strings_schema(&line_keys(items.len())))).await?;
        claude_parse(self.model, &v, items.len()).map_err(CloudErr::Other)
    }

    pub async fn search(&self, ru: &str) -> std::result::Result<(SearchQuery, f64), CloudErr> {
        let (o, usd) = self.ask(SEARCH_PROMPT, ru, strings_schema(&["q".to_string(), "ru".to_string()])).await?;
        Ok((search_parse(&o).map_err(CloudErr::Other)?, usd))
    }

    pub async fn ask(&self, system: &str, user: &str, schema: Value) -> std::result::Result<(Value, f64), CloudErr> {
        let v = self.call(&claude_body(self.model, system, user, schema)).await?;
        claude_object(self.model, &v).map_err(CloudErr::Other)
    }
}

const GEMINI: &str = "https://generativelanguage.googleapis.com/v1beta";

pub struct Gemini {
    http: reqwest::Client,
    key: String,
    pub model: String,
    think_ctl: std::sync::atomic::AtomicBool,
}

impl Gemini {
    pub async fn connect(key: String) -> Result<Self> {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(60)).build()?;
        let v: Value = http.get(format!("{GEMINI}/models?pageSize=200")).header("x-goog-api-key", &key).send().await?.json().await?;
        let model = pick_model(&v)?;
        Ok(Self { http, key, model, think_ctl: std::sync::atomic::AtomicBool::new(true) })
    }

    async fn translate(&self, system: &str, items: &[String]) -> std::result::Result<Vec<String>, CloudErr> {
        let think = self.think_ctl.load(std::sync::atomic::Ordering::Relaxed);
        let body = gemini_body(system, items, think);
        let req = self.http.post(format!("{GEMINI}/models/{}:generateContent", self.model)).header("x-goog-api-key", &self.key).json(&body);
        let (status, v) = post_json(req).await?;
        if status == 400 && think {
            self.think_ctl.store(false, std::sync::atomic::Ordering::Relaxed);
            return Err(CloudErr::Busy);
        }
        gemini_parse(status, &v, items.len())
    }
}

fn model_score(n: &str) -> (u8, i64) {
    let kind = if n.contains("flash-lite") { 2 } else if n.contains("flash") { 1 } else { 0 };
    let ver: f64 = n.trim_start_matches("gemini-").split('-').next().unwrap_or("0").parse().unwrap_or(0.0);
    (kind, (ver * 100.0) as i64)
}

const NOT_TEXT_MODELS: [&str; 6] = ["preview", "exp", "tts", "image", "live", "embedding"];

fn text_model(m: &Value) -> Option<String> {
    let methods = m["supportedGenerationMethods"].as_array()?;
    if !methods.iter().any(|x| x == "generateContent") {
        return None;
    }
    let n = m["name"].as_str()?.trim_start_matches("models/");
    let usable = n.starts_with("gemini-") && !NOT_TEXT_MODELS.iter().any(|x| n.contains(x));
    usable.then(|| n.to_string())
}

fn pick_model(v: &Value) -> Result<String> {
    if let Some(e) = v.get("error") {
        return Err(anyhow!("{}", e["message"].as_str().unwrap_or("ключ не принят")));
    }
    let mut names: Vec<String> = v["models"].as_array().into_iter().flatten().filter_map(text_model).collect();
    names.sort_by_key(|n| std::cmp::Reverse(model_score(n)));
    names.into_iter().find(|n| model_score(n).0 > 0).ok_or_else(|| anyhow!("нет моделей Flash для этого ключа"))
}

fn gemini_body(system: &str, items: &[String], think: bool) -> Value {
    let mut config = json!({
        "temperature": 0.2,
        "responseMimeType": "application/json",
        "responseSchema": {"type": "ARRAY", "items": {"type": "STRING"}}
    });
    if think {
        config["thinkingConfig"] = json!({"thinkingLevel": "low"});
    }
    json!({
        "systemInstruction": {"parts": [{"text": system}]},
        "contents": [{"role": "user", "parts": [{"text": serde_json::to_string(items).unwrap_or_default()}]}],
        "generationConfig": config
    })
}

fn gemini_error(status: u16, v: &Value) -> CloudErr {
    let msg = v["error"]["message"].as_str().unwrap_or("").to_string();
    match status {
        500 | 503 | 504 => CloudErr::Busy,
        429 if msg.to_lowercase().contains("per day") || msg.contains("PerDay") => CloudErr::Off(msg),
        429 => CloudErr::Busy,
        401 | 403 => CloudErr::Off(or_default(msg, "доступ запрещён")),
        _ => CloudErr::Other(anyhow!("HTTP {status}: {msg}")),
    }
}

fn gemini_parse(status: u16, v: &Value, n: usize) -> std::result::Result<Vec<String>, CloudErr> {
    if status >= 400 {
        return Err(gemini_error(status, v));
    }
    let text = v["candidates"][0]["content"]["parts"][0]["text"].as_str().unwrap_or("");
    let out: Vec<String> = serde_json::from_str(text).map_err(|e| CloudErr::Other(anyhow!("bad JSON: {e}")))?;
    if out.len() != n {
        return Err(CloudErr::Other(anyhow!("ответ {} строк вместо {}", out.len(), n)));
    }
    Ok(out)
}

const PACIFIC: i64 = -8;
const TASHKENT: i64 = 5;

pub struct Usage {
    path: PathBuf,
    offset_h: i64,
    pub limit: Option<u64>,
    day: u64,
    month: u32,
    pub requests: u64,
    pub strings: u64,
    pub usd_day: f64,
    pub usd_month: f64,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn day_at(secs: i64, offset_h: i64) -> u64 {
    ((secs + offset_h * 3600).max(0) / 86400) as u64
}

fn month_of(day: u64) -> u32 {
    let (y, m, _) = crate::money::civil(day);
    y as u32 * 100 + m
}

impl Usage {
    pub fn load(path: PathBuf, offset_h: i64, limit: Option<u64>) -> Self {
        let v: Value = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null);
        let mut u = Self {
            path,
            offset_h,
            limit,
            day: v["day"].as_u64().unwrap_or(0),
            month: v["month"].as_u64().unwrap_or(0) as u32,
            requests: v["requests"].as_u64().unwrap_or(0),
            strings: v["strings"].as_u64().unwrap_or(0),
            usd_day: v["usd_day"].as_f64().unwrap_or(0.0),
            usd_month: v["usd_month"].as_f64().unwrap_or(0.0),
        };
        u.roll(day_at(now_secs(), offset_h));
        u
    }

    fn roll(&mut self, day: u64) {
        if day != self.day {
            self.day = day;
            self.requests = 0;
            self.strings = 0;
            self.usd_day = 0.0;
        }
        let m = month_of(day);
        if m != self.month {
            self.month = m;
            self.usd_month = 0.0;
        }
    }

    pub fn add(&mut self, strings: usize) {
        self.roll(day_at(now_secs(), self.offset_h));
        self.requests += 1;
        self.strings += strings as u64;
        self.save();
    }

    pub fn spend(&mut self, usd: f64) {
        self.usd_day += usd;
        self.usd_month += usd;
        self.save();
    }

    fn save(&self) {
        let _ = std::fs::write(
            &self.path,
            json!({"day": self.day, "month": self.month, "requests": self.requests, "strings": self.strings,
                   "usd_day": self.usd_day, "usd_month": self.usd_month})
            .to_string(),
        );
    }

    pub fn until_reset(&self) -> Duration {
        let s = now_secs() + self.offset_h * 3600;
        Duration::from_secs((86400 - s.rem_euclid(86400)) as u64)
    }

    pub fn line(&mut self, name: &str) -> String {
        self.roll(day_at(now_secs(), self.offset_h));
        match self.limit {
            Some(limit) => {
                let left = self.until_reset().as_secs();
                format!(
                    "{name} сегодня: {} из ~{limit} запросов, {} строк; сброс через {}ч {:02}м",
                    self.requests, self.strings, left / 3600, left % 3600 / 60
                )
            }
            None => format!(
                "{name} сегодня: ${:.3} ({} запросов, {} строк), за месяц ${:.2}",
                self.usd_day, self.requests, self.strings, self.usd_month
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(text: &str, stop: &str) -> Value {
        json!({"stop_reason": stop, "content": [{"type": "text", "text": text}],
               "usage": {"input_tokens": 1_000_000, "output_tokens": 2_000_000}})
    }

    #[test]
    fn body_asks_for_one_required_key_per_line() {
        let items = ["删除".to_string(), "宝贝".to_string()];
        let b = claude_body("claude-haiku-5-5", "sys", &numbered(&items), strings_schema(&line_keys(items.len())));
        assert_eq!(b["output_config"]["format"]["schema"]["required"], json!(["1", "2"]));
        assert_eq!(b["messages"][0]["content"], json!(r#"{"1":"删除","2":"宝贝"}"#));
        assert!(b.get("temperature").is_none(), "sampling params are rejected on Haiku 5.5");
    }

    #[test]
    fn parse_returns_lines_in_order_and_cost() {
        let d = claude_parse("claude-haiku-5-5", &reply(r#"{"2":"Товар","1":"Удалить"}"#, "end_turn"), 2).unwrap();
        assert_eq!(d.lines, ["Удалить", "Товар"]);
        assert!((d.usd - (0.10 + 2.0 * 0.50)).abs() < 1e-9);
    }

    #[test]
    fn parse_rejects_missing_line_and_cut_reply() {
        assert!(claude_parse("claude-haiku-5-5", &reply(r#"{"1":"Удалить"}"#, "end_turn"), 2).is_err());
        assert!(claude_parse("claude-haiku-5-5", &reply(r#"{"1":"a","2":"b"}"#, "max_tokens"), 2).is_err());
        assert!(claude_parse("claude-haiku-5-5", &reply("", "refusal"), 2).is_err());
    }

    fn gemini() -> Gemini {
        Gemini { http: reqwest::Client::new(), key: "k".into(), model: "gemini-3.5-flash-lite".into(), think_ctl: std::sync::atomic::AtomicBool::new(true) }
    }

    #[test]
    fn each_cloud_has_its_name_model_pace_and_ledger() {
        let dir = std::env::temp_dir().join(format!("tbtr_cl_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let c = Cloud::Claude(Claude::new("k".into()).unwrap());
        let g = Cloud::Gemini(gemini());
        assert_eq!((c.name(), c.model()), ("Claude", "claude-haiku-5-5"));
        assert_eq!((g.name(), g.model()), ("Gemini", "gemini-3.5-flash-lite"));
        let (pc, pg) = (c.pace(), g.pace());
        assert_eq!((pc.batch, pc.workers, pc.gap), (10, 4, Duration::ZERO));
        assert_eq!((pg.batch, pg.workers, pg.gap), (80, 1, Duration::from_millis(4300)));
        assert!(pg.busy_wait > pc.busy_wait);
        assert_eq!((c.usage(&dir).limit, g.usage(&dir).limit), (None, Some(500)));
    }

    #[test]
    fn picks_the_newest_flash_lite_that_can_write_text() {
        let m = |n: &str, gen: bool| json!({"name": format!("models/{n}"), "supportedGenerationMethods": if gen { json!(["generateContent"]) } else { json!(["embedContent"]) }});
        let v = json!({"models": [m("gemini-2.5-flash", true), m("gemini-3.5-flash-lite", true), m("gemini-4.0-flash-lite-preview", true),
                                  m("gemini-9.0-flash-lite", false), m("gemini-3.0-pro", true), m("gemini-3.0-flash-lite-tts", true)]});
        assert_eq!(pick_model(&v).unwrap(), "gemini-3.5-flash-lite");
        assert!(pick_model(&json!({"models": [m("gemini-3.0-pro", true)]})).is_err());
        assert!(pick_model(&json!({"error": {"message": "API key not valid"}})).is_err());
    }

    #[test]
    fn gemini_body_asks_for_a_string_array_and_low_thinking_only_when_allowed() {
        let b = gemini_body("sys", &["宝贝".to_string()], true);
        assert_eq!(b["generationConfig"]["thinkingConfig"]["thinkingLevel"], "low");
        assert_eq!(b["contents"][0]["parts"][0]["text"], r#"["宝贝"]"#);
        assert!(gemini_body("sys", &[], false)["generationConfig"].get("thinkingConfig").is_none());
    }

    #[test]
    fn gemini_replies_map_to_lines_or_the_right_error() {
        let ok = json!({"candidates": [{"content": {"parts": [{"text": r#"["Товар","Удалить"]"#}]}}]});
        assert_eq!(gemini_parse(200, &ok, 2).ok().unwrap(), ["Товар", "Удалить"]);
        assert!(matches!(gemini_parse(200, &ok, 3), Err(CloudErr::Other(_))));
        let err = |m: &str| json!({"error": {"message": m}});
        assert!(matches!(gemini_parse(503, &err("overloaded"), 1), Err(CloudErr::Busy)));
        assert!(matches!(gemini_parse(429, &err("Quota exceeded per minute"), 1), Err(CloudErr::Busy)));
        assert!(matches!(gemini_parse(429, &err("Quota exceeded ... PerDay"), 1), Err(CloudErr::Off(_))));
        assert!(matches!(gemini_parse(403, &err(""), 1), Err(CloudErr::Off(m)) if m == "доступ запрещён"));
        assert!(matches!(gemini_parse(404, &err("no"), 1), Err(CloudErr::Other(_))));
    }

    #[test]
    fn key_file_skips_comments_and_the_placeholder() {
        let dir = std::env::temp_dir().join(format!("tbtr_key_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("x.key"), "# comment\n\nKEY\n  sk-test-123  \n").unwrap();
        assert_eq!(read_key(&dir, "x.key").as_deref(), Some("sk-test-123"));
        std::fs::write(dir.join("y.key"), "# only\nKEY\n").unwrap();
        assert_eq!(read_key(&dir, "y.key"), None);
        assert_eq!(read_key(&dir, "missing.key"), None);
    }

    #[test]
    fn ledger_line_shows_quota_for_gemini_and_dollars_for_claude() {
        let dir = std::env::temp_dir().join(format!("tbtr_line_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut g = Usage::load(dir.join("g.json"), PACIFIC, Some(500));
        g.add(80);
        assert!(g.line("Gemini").starts_with("Gemini сегодня: 1 из ~500 запросов, 80 строк; сброс через "));
        let mut c = Usage::load(dir.join("c.json"), TASHKENT, None);
        c.add(10);
        c.spend(0.0125);
        assert_eq!(c.line("Claude"), "Claude сегодня: $0.013 (1 запросов, 10 строк), за месяц $0.01");
        let again = Usage::load(dir.join("c.json"), TASHKENT, None);
        assert_eq!((again.requests, again.strings), (1, 10));
    }

    #[test]
    fn search_reply_gives_query_and_meaning() {
        let o = json!({"q": " 切尔西靴男 加绒 ", "ru": "челси мужские утеплённые"});
        let s = search_parse(&o).unwrap();
        assert_eq!((s.zh.as_str(), s.ru.as_str()), ("切尔西靴男 加绒", "челси мужские утеплённые"));
        assert!(search_parse(&json!({"q": "  ", "ru": "x"})).is_err(), "an empty query must not replace what the buyer typed");
    }

    #[test]
    fn errors_map_to_wait_stop_or_fallback() {
        let e = |s, m: &str| claude_error(s, &json!({"error": {"message": m}}));
        assert!(matches!(e(429, "concurrent requests"), CloudErr::Busy));
        assert!(matches!(e(529, "overloaded"), CloudErr::Busy));
        assert!(matches!(e(401, "invalid x-api-key"), CloudErr::Off(_)));
        assert!(matches!(e(400, "Your credit balance is too low"), CloudErr::Off(_)));
        assert!(matches!(e(400, "messages: bad field"), CloudErr::Other(_)));
    }

    #[test]
    fn month_boundaries() {
        assert_eq!(month_of(0), 197001);
        assert_eq!(month_of(20_405), 202511);
        assert_eq!(month_of(20_454), 202601);
        assert_eq!(month_of(20_453), 202512);
    }

    #[test]
    fn new_day_keeps_month_total_new_month_resets_it() {
        let dir = std::env::temp_dir().join(format!("tbtr_usage_{}", std::process::id()));
        let mut u = Usage::load(dir, TASHKENT, None);
        u.roll(20_440);
        u.spend(0.5);
        u.roll(20_441);
        assert_eq!((u.usd_day, u.usd_month), (0.0, 0.5));
        u.spend(0.25);
        u.roll(20_454);
        assert_eq!((u.usd_day, u.usd_month), (0.0, 0.0));
    }
}
