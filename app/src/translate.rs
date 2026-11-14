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
    rules: Vec<Rule>,
    pub cache: HashMap<String, String>,
    cache_path: PathBuf,
    review_path: PathBuf,
    strange_path: PathBuf,
    dirty: usize,
    http: reqwest::Client,
    model_url: String,
    pinyin_translit: Regex,
    latin_word: Regex,
    rubles: Regex,
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

        let rules = rules();

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
            rubles: Regex::new(r"(\d[\d.,]*)\s*₽").unwrap(),
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
        let mut d = Draft::default();
        let mut i = 0;
        while i < chars.len() {
            i += match (is_cjk(chars[i]), self.longest_term(&chars, i)) {
                (false, _) => d.other(chars[i]),
                (true, Some((len, v))) => d.term(len, v),
                (true, None) => d.unknown(),
            };
        }
        d.finish(total_cjk)
    }

    fn longest_term(&self, chars: &[char], i: usize) -> Option<(usize, &String)> {
        (2..=8.min(chars.len() - i)).rev().find_map(|len| {
            let w: String = chars[i..i + len].iter().collect();
            self.glossary.get(&w).map(|v| (len, v))
        })
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

    pub fn remember(&mut self, zh: &str, ru: &str, ms: u128, src: &str) -> String {
        let ru = &self.rubles.replace_all(ru, "¥$1").into_owned();
        self.cache.insert(zh.to_string(), ru.to_string());
        if let Some((key, t)) = clock_template(zh, ru) {
            self.cache.insert(key, t);
        }
        self.dirty += 1;
        let flags = self.flags(zh, ru);
        append(&self.review_path, &json!({"zh": zh, "ru": ru, "ms": ms, "src": src, "flags": flags}).to_string());
        if !flags.is_empty() {
            append(&self.strange_path, &format!("{}\t{}\t{}\t{}", zh, ru.replace(['\t', '\n'], " "), flags.join(", "), src));
        }
        if self.dirty >= 10 {
            self.save();
        }
        ru.to_string()
    }

    fn flags(&self, zh: &str, ru: &str) -> Vec<&'static str> {
        let zh_latin = zh.chars().filter(|c| c.is_ascii_alphabetic()).collect::<String>().to_lowercase();
        let (lz, lr) = (zh.chars().count(), ru.chars().count());
        [
            (ru.chars().any(is_cjk), "остался китайский"),
            (self.pinyin_translit.is_match(ru), "транслит"),
            (self.latin_word.find_iter(ru).any(|m| !zh_latin.contains(&m.as_str().to_lowercase())), "латиница не из оригинала"),
            (lz >= 4 && lr > lz * 7, "слишком длинно"),
        ]
        .into_iter()
        .filter_map(|(hit, name)| hit.then_some(name))
        .collect()
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
    let s = format!("{:.1}", v * 10.0);
    s.strip_suffix(".0").map(String::from).unwrap_or(s)
}

pub fn is_cjk(c: char) -> bool {
    ('\u{3400}'..='\u{9fff}').contains(&c)
}

#[derive(Default)]
struct Draft {
    parts: Vec<String>,
    latin: String,
    covered: usize,
}

impl Draft {
    fn flush(&mut self) {
        if !self.latin.is_empty() {
            self.parts.push(std::mem::take(&mut self.latin));
        }
    }

    fn other(&mut self, c: char) -> usize {
        if c.is_ascii_alphanumeric() || (!self.latin.is_empty() && matches!(c, '.' | '-' | '+')) {
            self.latin.push(c);
        } else {
            self.flush();
        }
        1
    }

    fn term(&mut self, len: usize, v: &str) -> usize {
        self.flush();
        if !v.is_empty() && !self.parts.iter().any(|p| p == v) {
            self.parts.push(v.to_string());
        }
        self.covered += len;
        len
    }

    fn unknown(&mut self) -> usize {
        self.flush();
        1
    }

    fn finish(mut self, total_cjk: usize) -> Option<String> {
        self.flush();
        if self.covered * 100 / total_cjk < 35 || self.parts.is_empty() {
            return None;
        }
        Some(format!("{} …", self.parts.join(" · ")))
    }
}

const CLOCK: &str = "⏱";

fn append(path: &Path, line: &str) {
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{line}");
    }
}

fn clock_template(zh: &str, ru: &str) -> Option<(String, String)> {
    let (key, times) = mask_clock(zh)?;
    if !times.iter().all(|x| ru.contains(x.as_str())) {
        return None;
    }
    let t = times.iter().fold(ru.to_string(), |t, x| t.replacen(x.as_str(), CLOCK, 1));
    Some((key, t))
}

fn count(c: &regex::Captures, n: usize, wan_at: usize) -> String {
    c.get(wan_at).map_or_else(|| c[n].to_string(), |_| format!("{} тыс.", wan(&c[n])))
}

fn plus(c: &regex::Captures, at: usize) -> &'static str {
    c.get(at).map_or("", |_| "+")
}

type Rule = (Regex, fn(&regex::Captures) -> String);

fn rules() -> Vec<Rule> {
    let r = |p: &str| Regex::new(p).unwrap();
    vec![
        (r(r"^已售\s*([\d.]+)(万)?(\+)?\s*件?$"), |c| format!("Продано {}{}", count(c, 1, 2), plus(c, 3))),
        (r(r"^([\d.]+)万(\+)?$"), |c| format!("{} тыс.{}", wan(&c[1]), plus(c, 2))),
        (r(r"^([\d.]+)(万)?\+?人(付款|购买|收货)$"), |c| format!("{}+ купили", count(c, 1, 2))),
        (r(r"^满(\d+)减(\d+)$"), |c| format!("−{} при покупке от {}", &c[2], &c[1])),
        (r(r"^满(\d+)元减(\d+)元$"), |c| format!("−¥{} при покупке от ¥{}", &c[2], &c[1])),
        (r(r"^再赚(\d+)淘金币可低至([\d.]+)元$"), |c| format!("Ещё {} Таобао-монет — цена от ¥{}", &c[1], &c[2])),
        (r(r"^官方立减(\d+)%$"), |c| format!("Скидка Taobao {}%", &c[1])),
        (r(r"^立减(\d+)%$"), |c| format!("Скидка {}%", &c[1])),
        (r(r"^逛(\d+)个商品\((\d+)/(\d+)\)$"), |c| format!("Посмотреть {} товара ({}/{})", &c[1], &c[2], &c[3])),
        (r(r"^([\d.]+)元$"), |c| format!("¥{}", &c[1])),
        (r(r"^共(\d+)件$"), |c| format!("Всего {} шт.", &c[1])),
        (r(r"^(\d+)件$"), |c| format!("{} шт.", &c[1])),
        (r(r"^(\d+)天$"), |c| format!("{} дн.", &c[1])),
        (r(r"^(\d+)小时$"), |c| format!("{} ч", &c[1])),
        (r(r"^(\d+)分钟$"), |c| format!("{} мин", &c[1])),
    ]
}

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
        tr_with("{}")
    }

    fn tr_with(glossary: &str) -> Translator {
        let dir = std::env::temp_dir().join(format!("tbtr_tr_{}_{:?}", std::process::id(), std::thread::current().id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("glossary.json"), glossary).unwrap();
        let _ = fs::remove_file(dir.join("cache.json"));
        Translator::load(&dir, "http://127.0.0.1:1").unwrap()
    }

    const G: &str = r#"{"马丁靴": "мартинсы", "男士": "мужские", "新款": "новинка", "宝贝": "товар", "亲": "", "张": "шт.", "亲亲": ""}"#;

    #[test]
    fn one_letter_and_empty_words_are_not_hints_for_the_model() {
        let t = tr_with(G);
        assert!(t.terms_for(&["一张亲亲纸".to_string()]).is_empty());
    }

    #[test]
    fn titles_are_long_lines_that_are_not_licences() {
        assert!(!Translator::is_title(&"字".repeat(20)));
        assert!(Translator::is_title(&"字".repeat(21)));
        for mark in ["证", "许可", "备案", "："] {
            assert!(!Translator::is_title(&format!("{}{mark}", "字".repeat(21))), "{mark}");
        }
    }

    #[test]
    fn four_known_characters_are_enough_for_a_draft() {
        assert_eq!(tr_with(G).draft("男士新款").as_deref(), Some("мужские · новинка …"));
    }

    #[test]
    fn draft_needs_at_least_35_percent_known() {
        let t = tr_with(G);
        let unknown13 = "一二三四五六七八九十百千万";
        assert!(t.draft(&format!("马丁靴男士新款{unknown13}")).is_some(), "7 of 20 is exactly 35%");
        assert!(t.draft(&format!("马丁靴男士新款{unknown13}亿")).is_none(), "7 of 21 is less");
    }

    #[test]
    fn unknown_chinese_separates_latin_words() {
        assert_eq!(tr_with(G).draft("AB未CD男士新款").as_deref(), Some("AB · CD · мужские · новинка …"));
    }

    #[test]
    fn different_searches_do_not_share_a_cache_entry() {
        let mut t = tr();
        t.remember_search("сумка", "包", "сумка");
        assert_eq!(t.search_cached("ботинки"), None);
        t.save();
        let again = Translator::load(t.cache_path.parent().unwrap(), "http://127.0.0.1:1").unwrap();
        assert_eq!(again.search_cached("сумка"), Some(("包".to_string(), "сумка".to_string())), "a search alone is worth saving");
    }

    #[test]
    fn cache_saves_itself_every_ten_new_answers() {
        let mut t = tr();
        for i in 0..9 {
            t.remember(&format!("字{i}"), "x", 1, "cloud");
        }
        assert!(!t.cache_path.exists());
        t.remember("字9", "x", 1, "cloud");
        assert!(t.cache_path.exists());
    }

    #[test]
    fn too_long_means_more_than_seven_times_the_chinese() {
        let t = tr();
        assert!(t.flags("新款上市", &"я".repeat(28)).is_empty());
        assert_eq!(t.flags("新款上市", &"я".repeat(29)), ["слишком длинно"]);
        assert!(t.flags("新款上", &"я".repeat(40)).is_empty(), "short lines are never too long");
    }

    #[test]
    fn number_rules_translate_counts_discounts_and_times() {
        let t = tr();
        let cases = [
            ("已售65件", "Продано 65"),
            ("已售 2.5万+件", "Продано 25 тыс.+"),
            ("14万+", "140 тыс.+"),
            ("3万", "30 тыс."),
            ("1.25万", "12.5 тыс."),
            ("600+人付款", "600+ купили"),
            ("1.2万人收货", "12 тыс.+ купили"),
            ("满3减1", "−1 при покупке от 3"),
            ("满200元减20元", "−¥20 при покупке от ¥200"),
            ("再赚185淘金币可低至83.0元", "Ещё 185 Таобао-монет — цена от ¥83.0"),
            ("官方立减15%", "Скидка Taobao 15%"),
            ("立减5%", "Скидка 5%"),
            ("逛3个商品(0/3)", "Посмотреть 3 товара (0/3)"),
            ("59.9元", "¥59.9"),
            ("共4件", "Всего 4 шт."),
            ("4件", "4 шт."),
            ("7天", "7 дн."),
            ("12小时", "12 ч"),
            ("30分钟", "30 мин"),
        ];
        for (zh, ru) in cases {
            assert_eq!(fast(&t, zh).as_deref(), Some(ru), "{zh}");
        }
    }

    #[test]
    fn strange_translations_are_flagged_for_review() {
        let t = tr();
        assert!(t.flags("宝贝", "Товар").is_empty());
        assert_eq!(t.flags("宝贝", "Товар 宝"), ["остался китайский"]);
        assert_eq!(t.flags("莆田鞋", "Путянь цзы"), ["транслит"]);
        assert_eq!(t.flags("耳机", "Наушники Apple"), ["латиница не из оригинала"]);
        assert!(t.flags("苹果Apple耳机", "Наушники Apple").is_empty());
        assert_eq!(t.flags("新款上市", &"очень ".repeat(6)), ["слишком длинно"]);
    }

    #[test]
    fn cache_is_saved_and_loaded_back() {
        let mut t = tr();
        t.save();
        assert!(!t.cache_path.exists(), "nothing new, nothing written");
        t.remember("别的", "Другое", 1, "cloud");
        t.save();
        let again = Translator::load(t.cache_path.parent().unwrap(), "http://127.0.0.1:1").unwrap();
        assert_eq!(again.cache.get("别的").map(String::as_str), Some("Другое"));
        assert_eq!(t.dirty, 0);
    }

    #[test]
    fn yuan_amounts_never_turn_into_rubles() {
        let mut t = tr();
        assert_eq!(t.remember("官方立减30元", "Скидка Taobao 30 ₽", 1, "cloud"), "Скидка Taobao ¥30");
        assert_eq!(t.cache.get("官方立减30元").map(String::as_str), Some("Скидка Taobao ¥30"));
        assert_eq!(t.remember("淘金币抵1.5元", "Таобао-монеты: скидка 1,5 ₽", 1, "cloud"), "Таобао-монеты: скидка ¥1,5");
        assert_eq!(t.remember("别的", "Другое", 1, "cloud"), "Другое");
    }

    #[test]
    fn remember_logs_every_answer_and_the_strange_ones_separately() {
        let mut t = tr();
        let _ = fs::remove_file(&t.review_path);
        let _ = fs::remove_file(&t.strange_path);
        t.remember("宝贝", "Товар", 5, "cloud");
        t.remember("耳机", "Наушники Apple", 6, "local");
        let review = fs::read_to_string(&t.review_path).unwrap();
        assert_eq!(review.lines().count(), 2);
        let strange = fs::read_to_string(&t.strange_path).unwrap();
        assert_eq!(strange, "耳机\tНаушники Apple\tлатиница не из оригинала\tlocal\n");
    }

    #[test]
    fn draft_strings_known_words_together() {
        let t = tr_with(G);
        assert_eq!(t.draft("马丁靴男士2025新款").as_deref(), Some("мартинсы · мужские · 2025 · новинка …"));
        assert_eq!(t.draft("马丁靴 iPhone-15 男士").as_deref(), Some("мартинсы · iPhone-15 · мужские …"));
        assert_eq!(t.draft("男士马丁皮靴高帮"), None, "less than 35% of the Chinese is known");
        assert_eq!(t.draft("马丁靴"), None, "too short for a draft");
        assert_eq!(t.draft("男士男士宝贝").as_deref(), Some("мужские · товар …"));
    }

    #[test]
    fn prompt_adds_known_terms_and_uses_the_catalog_form_for_titles() {
        let t = tr_with(G);
        assert_eq!(t.prompt("删除"), "把下面的文本翻译成俄语，不要额外解释。\n\n删除");
        assert!(t.prompt("男士专区").starts_with("参考下面的翻译：\n男士 翻译成 мужские\n\n把下面的文本翻译成俄语"));
        let title = "马丁靴男士2025冬季加绒保暖高帮切尔西靴英伦风";
        assert!(t.prompt(title).contains("译成简短的俄语商品目录名称"));
        assert!(!t.prompt("增值电信业务经营许可证：浙B2-20080224-1号码号码").contains("商品目录"), "licences are not product titles");
    }

    #[test]
    fn terms_for_lists_glossary_words_found_in_the_batch_longest_first() {
        let t = tr_with(G);
        let got = t.terms_for(&["马丁靴男士".to_string(), "删除".to_string()]);
        assert_eq!(got, [("马丁靴".to_string(), "мартинсы".to_string()), ("男士".to_string(), "мужские".to_string())]);
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
