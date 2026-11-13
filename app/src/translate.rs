use anyhow::{Context, Result};
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub struct Translator {
    glossary: HashMap<String, String>,
    terms: Vec<String>,
    rules: Vec<(Regex, fn(&regex::Captures) -> String)>,
    pub cache: HashMap<String, String>,
    cache_path: PathBuf,
    review_path: PathBuf,
    strange_path: PathBuf,
    dirty: usize,
    http: reqwest::Client,
    model_url: String,
    pinyin_translit: Regex,
    latin_word: Regex,
}

pub enum Hit {
    Fast(String),
    Miss,
}

impl Translator {
    pub fn load(data: &Path, model_url: &str) -> Result<Self> {
        let glossary: HashMap<String, String> =
            serde_json::from_str(&fs::read_to_string(data.join("glossary.json")).context("glossary.json")?)?;
        let mut terms: Vec<String> = glossary
            .iter()
            .filter(|(k, v)| k.chars().count() >= 2 && !v.is_empty())
            .map(|(k, _)| k.clone())
            .collect();
        terms.sort_by_key(|k| std::cmp::Reverse(k.chars().count()));

        let cache_path = data.join("cache.json");
        let cache = fs::read_to_string(&cache_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        let rules: Vec<(Regex, fn(&regex::Captures) -> String)> = vec![
            (Regex::new(r"^已售\s*([\d.]+)(万)?(\+)?\s*件?$").unwrap(), |c| {
                let n = if c.get(2).is_some() { format!("{} тыс.", wan(&c[1])) } else { c[1].to_string() };
                format!("Продано {}{}", n, c.get(3).map_or("", |_| "+"))
            }),
            (Regex::new(r"^([\d.]+)万(\+)?$").unwrap(), |c| format!("{} тыс.{}", wan(&c[1]), c.get(2).map_or("", |_| "+"))),
            (Regex::new(r"^([\d.]+)(万)?\+?人(付款|购买|收货)$").unwrap(), |c| {
                let n = if c.get(2).is_some() { format!("{} тыс.", wan(&c[1])) } else { c[1].to_string() };
                format!("{}+ купили", n)
            }),
            (Regex::new(r"^满(\d+)减(\d+)$").unwrap(), |c| format!("−{} при покупке от {}", &c[2], &c[1])),
            (Regex::new(r"^满(\d+)元减(\d+)元$").unwrap(), |c| format!("−¥{} при покупке от ¥{}", &c[2], &c[1])),
            (Regex::new(r"^再赚(\d+)淘金币可低至([\d.]+)元$").unwrap(), |c| format!("Ещё {} Таобао-монет — цена от ¥{}", &c[1], &c[2])),
            (Regex::new(r"^官方立减(\d+)%$").unwrap(), |c| format!("Скидка Taobao {}%", &c[1])),
            (Regex::new(r"^立减(\d+)%$").unwrap(), |c| format!("Скидка {}%", &c[1])),
            (Regex::new(r"^逛(\d+)个商品\((\d+)/(\d+)\)$").unwrap(), |c| format!("Посмотреть {} товара ({}/{})", &c[1], &c[2], &c[3])),
            (Regex::new(r"^([\d.]+)元$").unwrap(), |c| format!("¥{}", &c[1])),
            (Regex::new(r"^共(\d+)件$").unwrap(), |c| format!("Всего {} шт.", &c[1])),
            (Regex::new(r"^(\d+)件$").unwrap(), |c| format!("{} шт.", &c[1])),
            (Regex::new(r"^(\d+)天$").unwrap(), |c| format!("{} дн.", &c[1])),
            (Regex::new(r"^(\d+)小时$").unwrap(), |c| format!("{} ч", &c[1])),
            (Regex::new(r"^(\d+)分钟$").unwrap(), |c| format!("{} мин", &c[1])),
        ];

        Ok(Self {
            glossary,
            terms,
            rules,
            cache,
            cache_path,
            review_path: data.join("review.jsonl"),
            strange_path: data.join("strange.tsv"),
            dirty: 0,
            http: reqwest::Client::new(),
            model_url: model_url.to_string(),
            pinyin_translit: Regex::new(r"(?i)(цз|сюн|сюй|сяо|чж|чэн|ньс|бэй|хуэй|юнь|жэнь|цю|ця|цзя|ли?нь|гуань|чуань|шэн|дзы|цзы)").unwrap(),
            latin_word: Regex::new(r"[A-Za-z]{4,}").unwrap(),
        })
    }

    pub fn fast(&self, s: &str) -> Hit {
        if let Some(v) = self.glossary.get(s) {
            return Hit::Fast(if v.is_empty() { s.to_string() } else { v.clone() });
        }
        if let Some(v) = self.cache.get(s) {
            return Hit::Fast(v.clone());
        }
        if let Some((key, times)) = mask_clock(s) {
            if let Some(v) = self.cache.get(&key).and_then(|t| unmask_clock(t, &times)) {
                return Hit::Fast(v);
            }
        }
        for (re, f) in &self.rules {
            if let Some(c) = re.captures(s) {
                return Hit::Fast(f(&c));
            }
        }
        Hit::Miss
    }

    fn is_title(s: &str) -> bool {
        s.chars().count() > 20 && !(s.contains('证') || s.contains("许可") || s.contains("备案") || s.contains('：'))
    }

    pub fn terms_for(&self, items: &[String]) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for k in &self.terms {
            if items.iter().any(|s| s.contains(k.as_str())) {
                out.push((k.clone(), self.glossary[k].clone()));
            }
        }
        out
    }

    pub fn draft(&self, s: &str) -> Option<String> {
        let chars: Vec<char> = s.chars().collect();
        let total_cjk = chars.iter().filter(|c| is_cjk(**c)).count();
        if total_cjk < 4 {
            return None;
        }
        let mut parts: Vec<String> = Vec::new();
        let mut covered = 0;
        let mut i = 0;
        let mut latin = String::new();
        while i < chars.len() {
            let c = chars[i];
            if !is_cjk(c) {
                if c.is_ascii_alphanumeric() || (!latin.is_empty() && matches!(c, '.' | '-' | '+')) {
                    latin.push(c);
                } else if !latin.is_empty() {
                    parts.push(std::mem::take(&mut latin));
                }
                i += 1;
                continue;
            }
            if !latin.is_empty() {
                parts.push(std::mem::take(&mut latin));
            }
            let mut best = None;
            for len in (2..=8.min(chars.len() - i)).rev() {
                let w: String = chars[i..i + len].iter().collect();
                if let Some(v) = self.glossary.get(&w) {
                    best = Some((len, v.clone()));
                    break;
                }
            }
            match best {
                Some((len, v)) => {
                    if !v.is_empty() && !parts.contains(&v) {
                        parts.push(v);
                    }
                    covered += len;
                    i += len;
                }
                None => i += 1,
            }
        }
        if !latin.is_empty() {
            parts.push(latin);
        }
        if covered * 100 / total_cjk < 35 || parts.is_empty() {
            return None;
        }
        Some(format!("{} …", parts.join(" · ")))
    }

    pub fn client(&self) -> (reqwest::Client, String) {
        (self.http.clone(), self.model_url.clone())
    }

    pub fn prompt(&self, s: &str) -> String {
        let mut used = Vec::new();
        let mut rest = s.to_string();
        for k in &self.terms {
            if rest.contains(k.as_str()) {
                used.push(k.as_str());
                rest = rest.replace(k.as_str(), " ");
            }
        }
        let terms = if used.is_empty() {
            String::new()
        } else {
            let lines: Vec<String> = used.iter().map(|k| format!("{} 翻译成 {}", k, self.glossary[*k])).collect();
            format!("参考下面的翻译：\n{}\n\n", lines.join("\n"))
        };
        if Self::is_title(s) {
            format!("{terms}上文是参考信息。下面是淘宝商品标题，译成简短的俄语商品目录名称：关键属性用逗号分隔，不超过12个词，只译原文信息，不要扩写成句子，不要添加原文没有的内容。\n参考上面的信息，把下面的文本翻译成俄语，注意不需要翻译上文，也不要额外解释：\n{s}")
        } else {
            format!("{terms}把下面的文本翻译成俄语，不要额外解释。\n\n{s}")
        }
    }

    pub async fn model(http: &reqwest::Client, url: &str, prompt: String, s: &str) -> Result<(String, u128)> {
        let t = Instant::now();
        let body = json!({
            "temperature": 0.2, "top_k": 20, "top_p": 0.6, "repeat_penalty": 1.05, "max_tokens": 160,
            "messages": [{"role": "user", "content": prompt}]
        });
        let v: Value = http.post(url).json(&body).send().await?.json().await?;
        let mut out = v["choices"][0]["message"]["content"].as_str().unwrap_or("").trim().to_string();
        if !s.ends_with('。') && !s.ends_with('.') && out.ends_with('.') && !out.ends_with("..") {
            out.pop();
        }
        if out.is_empty() {
            out = s.to_string();
        }
        Ok((out, t.elapsed().as_millis()))
    }

    pub fn search_cached(&self, ru: &str) -> Option<(String, String)> {
        let v = self.cache.get(&search_key(ru))?;
        let (zh, m) = v.split_once('\t').unwrap_or((v.as_str(), ""));
        Some((zh.to_string(), m.to_string()))
    }

    pub fn remember_search(&mut self, ru: &str, zh: &str, meaning: &str) {
        self.cache.insert(search_key(ru), format!("{zh}\t{meaning}"));
        self.dirty += 1;
    }

    pub fn remember(&mut self, zh: &str, ru: &str, ms: u128, src: &str) {
        self.cache.insert(zh.to_string(), ru.to_string());
        if let Some((key, times)) = mask_clock(zh) {
            let mut t = ru.to_string();
            if times.iter().all(|x| t.contains(x.as_str())) {
                for x in &times {
                    t = t.replacen(x.as_str(), CLOCK, 1);
                }
                self.cache.insert(key, t);
            }
        }
        self.dirty += 1;

        let mut flags = Vec::new();
        if ru.chars().any(is_cjk) {
            flags.push("остался китайский");
        }
        if self.pinyin_translit.is_match(ru) {
            flags.push("транслит");
        }
        let zh_latin: String = zh.chars().filter(|c| c.is_ascii_alphabetic()).collect::<String>().to_lowercase();
        if self.latin_word.find_iter(ru).any(|m| !zh_latin.contains(&m.as_str().to_lowercase())) {
            flags.push("латиница не из оригинала");
        }
        let (lz, lr) = (zh.chars().count(), ru.chars().count());
        if lz >= 4 && lr > lz * 7 {
            flags.push("слишком длинно");
        }

        let line = json!({"zh": zh, "ru": ru, "ms": ms, "src": src, "flags": flags});
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&self.review_path) {
            let _ = writeln!(f, "{}", line);
        }
        if !flags.is_empty() {
            if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&self.strange_path) {
                let _ = writeln!(f, "{}\t{}\t{}\t{}", zh, ru.replace(['\t', '\n'], " "), flags.join(", "), src);
            }
        }
        if self.dirty >= 10 {
            self.save();
        }
    }

    pub fn save(&mut self) {
        if self.dirty == 0 {
            return;
        }
        let tmp = self.cache_path.with_extension("json.tmp");
        if let Ok(s) = serde_json::to_string_pretty(&self.cache) {
            if fs::write(&tmp, s).is_ok() {
                let _ = fs::rename(&tmp, &self.cache_path);
                self.dirty = 0;
            }
        }
    }
}

fn wan(n: &str) -> String {
    let v: f64 = n.parse().unwrap_or(0.0);
    let t = v * 10.0;
    if (t - t.round()).abs() < 1e-9 { format!("{}", t as i64) } else { format!("{:.1}", t) }
}

pub fn is_cjk(c: char) -> bool {
    ('\u{3400}'..='\u{9fff}').contains(&c)
}

const CLOCK: &str = "⏱";

fn search_key(ru: &str) -> String {
    format!("поиск:{}", ru.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase())
}

fn mask_clock(s: &str) -> Option<(String, Vec<String>)> {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\d{1,2}:\d{2}(?::\d{2})?").unwrap());
    let times: Vec<String> = re.find_iter(s).map(|m| m.as_str().to_string()).collect();
    if times.is_empty() {
        return None;
    }
    Some((re.replace_all(s, CLOCK).into_owned(), times))
}

fn unmask_clock(t: &str, times: &[String]) -> Option<String> {
    if t.matches(CLOCK).count() != times.len() {
        return None;
    }
    let mut out = t.to_string();
    for x in times {
        out = out.replacen(CLOCK, x, 1);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tr() -> Translator {
        let dir = std::env::temp_dir().join(format!("tbtr_tr_{}_{:?}", std::process::id(), std::thread::current().id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("glossary.json"), "{}").unwrap();
        let _ = fs::remove_file(dir.join("cache.json"));
        Translator::load(&dir, "http://127.0.0.1:1").unwrap()
    }

    fn fast(t: &Translator, s: &str) -> Option<String> {
        match t.fast(s) {
            Hit::Fast(x) => Some(x),
            Hit::Miss => None,
        }
    }

    #[test]
    fn countdown_is_translated_once_for_every_tick() {
        let mut t = tr();
        assert_eq!(fast(&t, "巅峰返场 08:13:45 后失效"), None);
        t.remember("巅峰返场 08:13:45 后失效", "Скидка действует до 08:13:45", 0, "cloud");
        assert_eq!(fast(&t, "巅峰返场 08:13:44 后失效").as_deref(), Some("Скидка действует до 08:13:44"));
        assert_eq!(fast(&t, "巅峰返场 23:01 后失效").as_deref(), Some("Скидка действует до 23:01"));
    }

    #[test]
    fn countdown_template_needs_the_time_in_the_translation() {
        let mut t = tr();
        t.remember("距结束 01:02:03", "Скоро конец", 0, "cloud");
        assert_eq!(fast(&t, "距结束 01:02:04"), None);
    }

    #[test]
    fn search_is_remembered_whatever_the_spacing_and_case() {
        let mut t = tr();
        assert!(t.search_cached("Челси мужские").is_none());
        t.remember_search("Челси мужские", "切尔西靴男", "челси мужские");
        assert_eq!(t.search_cached("  челси   МУЖСКИЕ "), Some(("切尔西靴男".to_string(), "челси мужские".to_string())));
    }
}
