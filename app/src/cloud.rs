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

fn claude_body(model: &str, system: &str, user: &str, keys: &[String]) -> Value {
    let props: serde_json::Map<String, Value> = keys.iter().map(|k| (k.clone(), json!({"type": "string"}))).collect();
    json!({
        "model": model,
        "max_tokens": 8000,
        "system": system,
        "output_config": {
            "effort": "low",
            "format": {"type": "json_schema", "schema": {
                "type": "object", "properties": props, "required": keys, "additionalProperties": false
            }}
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

fn claude_error(status: u16, v: &Value) -> CloudErr {
    let msg = v["error"]["message"].as_str().unwrap_or("").to_string();
    let low = msg.to_lowercase();
    match status {
        429 | 500 | 502 | 503 | 504 | 529 => CloudErr::Busy,
        401 | 403 => CloudErr::Off(if msg.is_empty() { "ключ не принят".into() } else { msg }),
        400 if low.contains("credit") || low.contains("billing") || low.contains("spend") || low.contains("limit") => CloudErr::Off(msg),
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
        let r = self
            .http
            .post(ANTHROPIC)
            .header("x-api-key", &self.key)
            .header("anthropic-version", "2026-10-21")
            .json(body)
            .send()
            .await
            .map_err(|e| CloudErr::Other(e.into()))?;
        let status = r.status().as_u16();
        let v: Value = r.json().await.map_err(|e| CloudErr::Other(e.into()))?;
        if status >= 400 {
            return Err(claude_error(status, &v));
        }
        Ok(v)
    }

    async fn translate(&self, system: &str, items: &[String]) -> std::result::Result<Done, CloudErr> {
        let v = self.call(&claude_body(self.model, system, &numbered(items), &line_keys(items.len()))).await?;
        claude_parse(self.model, &v, items.len()).map_err(CloudErr::Other)
    }

    pub async fn search(&self, ru: &str) -> std::result::Result<(SearchQuery, f64), CloudErr> {
        let keys = ["q".to_string(), "ru".to_string()];
        let v = self.call(&claude_body(self.model, SEARCH_PROMPT, ru, &keys)).await?;
        let (o, usd) = claude_object(self.model, &v).map_err(CloudErr::Other)?;
        Ok((search_parse(&o).map_err(CloudErr::Other)?, usd))
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
        let v: Value = http
            .get(format!("{GEMINI}/models?pageSize=200"))
            .header("x-goog-api-key", &key)
            .send()
            .await?
            .json()
            .await?;
        if let Some(e) = v.get("error") {
            return Err(anyhow!("{}", e["message"].as_str().unwrap_or("ключ не принят")));
        }
        let mut names: Vec<String> = v["models"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|m| {
                m["supportedGenerationMethods"].as_array().map_or(false, |a| a.iter().any(|x| x == "generateContent"))
            })
            .filter_map(|m| m["name"].as_str().map(|n| n.trim_start_matches("models/").to_string()))
            .filter(|n| n.starts_with("gemini-") && !n.contains("preview") && !n.contains("exp") && !n.contains("tts") && !n.contains("image") && !n.contains("live") && !n.contains("embedding"))
            .collect();
        let score = |n: &str| -> (u8, i64) {
            let kind = if n.contains("flash-lite") { 2 } else if n.contains("flash") { 1 } else { 0 };
            let ver: f64 = n.trim_start_matches("gemini-").split('-').next().unwrap_or("0").parse().unwrap_or(0.0);
            (kind, (ver * 100.0) as i64)
        };
        names.sort_by_key(|n| std::cmp::Reverse(score(n)));
        let model = names.into_iter().find(|n| score(n).0 > 0).ok_or_else(|| anyhow!("нет моделей Flash для этого ключа"))?;
        Ok(Self { http, key, model, think_ctl: std::sync::atomic::AtomicBool::new(true) })
    }

    async fn translate(&self, system: &str, items: &[String]) -> std::result::Result<Vec<String>, CloudErr> {
        let mut body = json!({
            "systemInstruction": {"parts": [{"text": system}]},
            "contents": [{"role": "user", "parts": [{"text": serde_json::to_string(items).unwrap()}]}],
            "generationConfig": {
                "temperature": 0.2,
                "responseMimeType": "application/json",
                "responseSchema": {"type": "ARRAY", "items": {"type": "STRING"}},
                "thinkingConfig": {"thinkingLevel": "low"}
            }
        });
        if !self.think_ctl.load(std::sync::atomic::Ordering::Relaxed) {
            if let Some(g) = body["generationConfig"].as_object_mut() {
                g.remove("thinkingConfig");
            }
        }
        let r = self
            .http
            .post(format!("{GEMINI}/models/{}:generateContent", self.model))
            .header("x-goog-api-key", &self.key)
            .json(&body)
            .send()
            .await
            .map_err(|e| CloudErr::Other(e.into()))?;
        let status = r.status().as_u16();
        let v: Value = r.json().await.map_err(|e| CloudErr::Other(e.into()))?;
        if status == 503 || status == 500 || status == 504 {
            return Err(CloudErr::Busy);
        }
        if status == 429 {
            let msg = v["error"]["message"].as_str().unwrap_or("").to_string();
            return Err(if msg.to_lowercase().contains("per day") || msg.contains("PerDay") { CloudErr::Off(msg) } else { CloudErr::Busy });
        }
        if status == 400 && self.think_ctl.load(std::sync::atomic::Ordering::Relaxed) {
            self.think_ctl.store(false, std::sync::atomic::Ordering::Relaxed);
            return Err(CloudErr::Busy);
        }
        if status == 401 || status == 403 {
            return Err(CloudErr::Off(v["error"]["message"].as_str().unwrap_or("доступ запрещён").to_string()));
        }
        if status >= 400 {
            return Err(CloudErr::Other(anyhow!("HTTP {status}: {}", v["error"]["message"].as_str().unwrap_or(""))));
        }
        let text = v["candidates"][0]["content"]["parts"][0]["text"].as_str().unwrap_or("");
        let out: Vec<String> = serde_json::from_str(text).map_err(|e| CloudErr::Other(anyhow!("bad JSON: {e}")))?;
        if out.len() != items.len() {
            return Err(CloudErr::Other(anyhow!("ответ {} строк вместо {}", out.len(), items.len())));
        }
        Ok(out)
    }
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
    let z = day as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y * 100 + m) as u32
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
        let b = claude_body("claude-haiku-5-5", "sys", &numbered(&items), &line_keys(items.len()));
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
