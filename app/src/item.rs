use crate::money::{cargo_uzs, price_uzs, sum, Rates, Settings};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Grade {
    Good,
    Ok,
    Bad,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Verdict {
    pub grade: Grade,
    pub why: String,
    pub complaints: String,
    pub price_cny: Option<f64>,
    pub weight_g: f64,
    pub weight_basis: String,
    pub banned: Option<String>,
}

pub const ITEM_PROMPT: &str = "Ты помогаешь покупателю из Ташкента оценить товар на Taobao по тексту его страницы (китайский, как есть). \
Оцени только по тому, что есть в тексте. Части оценки: отзывы на товар (доля хороших, на что жалуются) 35%, \
магазин (оценки описания, сервиса и доставки, возраст, значки вроде 天猫 или 金牌卖家) 25%, число продаж 20%, \
адекватность цены для такого товара 20%. Если данных мало, так и скажи и не ставь good. \
Все тексты в ответе только по-русски, без китайских иероглифов: 天猫 пиши как Tmall, 金牌卖家 как «золотой продавец», теги отзывов переводи. \
grade: good, ok или bad. why: одна короткая строка по-русски с цифрами со страницы. \
complaints: на что жалуются покупатели, коротко по-русски, или пустая строка. \
price_cny: основная цена товара в юанях (со скидкой, если она показана), или null. \
weight_g: примерный вес одной штуки с упаковкой для карго, в граммах. weight_basis: на чём основан вес, 2–5 слов. \
banned: если товар нельзя везти авиа-карго Top Cargo, укажи причину по-русски, иначе пустая строка. Нельзя: еда; лекарства и \
медицинские изделия; телефоны, сим-карты, смарт-часы, компьютеры, ноутбуки, наушники; золото, серебро, драгоценности; оружие, табак, \
наркотики; жидкости и порошки; дроны; военное; 18+; религиозные предметы; всё с батареей, аккумулятором или магнитом; \
зажигалки, спички и всё легковоспламеняющееся; сирены и мигалки.";

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "grade": {"type": "string", "enum": ["good", "ok", "bad"]},
            "why": {"type": "string", "description": "по-русски, без китайских иероглифов"},
            "complaints": {"type": "string", "description": "по-русски, без китайских иероглифов: теги вроде 尺码偏小 переведи («маломерит»)"},
            "price_cny": {"type": ["number", "null"]},
            "weight_g": {"type": "number"},
            "weight_basis": {"type": "string"},
            "banned": {"type": "string"}
        },
        "required": ["grade", "why", "complaints", "price_cny", "weight_g", "weight_basis", "banned"],
        "additionalProperties": false
    })
}

impl Grade {
    fn parse(s: Option<&str>) -> Result<Self> {
        match s {
            Some("good") => Ok(Grade::Good),
            Some("ok") => Ok(Grade::Ok),
            Some("bad") => Ok(Grade::Bad),
            g => Err(anyhow!("непонятная оценка: {g:?}")),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Grade::Good => "good",
            Grade::Ok => "ok",
            Grade::Bad => "bad",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Grade::Good => "✅ Хорошо",
            Grade::Ok => "🟡 Средне",
            Grade::Bad => "❌ Плохо",
        }
    }
}

fn weight(v: &Value) -> Result<f64> {
    v.as_f64().filter(|w| *w > 0.0 && *w < 100_000.0).ok_or_else(|| anyhow!("странный вес"))
}

pub fn parse_verdict(o: &Value) -> Result<Verdict> {
    let grade = Grade::parse(o["grade"].as_str())?;
    let weight_g = weight(&o["weight_g"])?;
    let text = |k: &str| o[k].as_str().unwrap_or("").trim().to_string();
    let banned = Some(text("banned")).filter(|b| !b.is_empty());
    Ok(Verdict {
        grade,
        why: text("why"),
        complaints: text("complaints"),
        price_cny: o["price_cny"].as_f64().filter(|p| *p > 0.0),
        weight_g,
        weight_basis: text("weight_basis"),
        banned,
    })
}

fn han_runs(s: &str) -> Vec<String> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"\p{Han}+").unwrap()).find_iter(s).map(|m| m.as_str().to_string()).collect()
}

impl Verdict {
    pub fn chinese_left(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for f in [&self.why, &self.complaints, &self.weight_basis] {
            for r in han_runs(f) {
                if !out.contains(&r) {
                    out.push(r);
                }
            }
        }
        for r in self.banned.iter().flat_map(|b| han_runs(b)) {
            if !out.contains(&r) {
                out.push(r);
            }
        }
        out
    }

    pub fn replace(&mut self, pairs: &[(String, String)]) {
        let mut sorted: Vec<&(String, String)> = pairs.iter().collect();
        sorted.sort_by_key(|(zh, _)| std::cmp::Reverse(zh.chars().count()));
        for f in [&mut self.why, &mut self.complaints, &mut self.weight_basis].into_iter().chain(self.banned.as_mut()) {
            for (zh, ru) in &sorted {
                *f = f.replace(zh.as_str(), ru.as_str());
            }
        }
    }
}

pub fn digest(text: &str, limit: usize) -> String {
    let mut seen = std::collections::HashSet::new();
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty() && seen.insert(*l)).collect();
    lines.join("\n").chars().take(limit).collect()
}

fn price_line(v: &Verdict, rates: Option<&Rates>, s: &Settings) -> Value {
    match (v.price_cny, rates) {
        (Some(p), Some(r)) => json!(format!("¥{p} · {}", sum(price_uzs(p, r, s)))),
        (Some(p), None) => json!(format!("¥{p}")),
        _ => Value::Null,
    }
}

fn cargo_line(v: &Verdict, rates: Option<&Rates>, s: &Settings) -> String {
    let tariff = format!("{}, ${}/кг", s.cargo_name, s.cargo_usd_per_kg);
    match rates {
        Some(r) => format!("≈ {} ({tariff})", sum(cargo_uzs(v.weight_g, r, s))),
        None => format!("~${:.2} ({tariff})", v.weight_g / 1000.0 * s.cargo_usd_per_kg),
    }
}

fn total_line(v: &Verdict, rates: Option<&Rates>, s: &Settings) -> Value {
    match (v.price_cny, rates) {
        (Some(p), Some(r)) => json!(format!("≈ {} с доставкой", sum(price_uzs(p, r, s) + cargo_uzs(v.weight_g, r, s)))),
        _ => Value::Null,
    }
}

pub fn card(v: &Verdict, rates: Option<&Rates>, s: &Settings) -> Value {
    let (price, cargo, total) = (price_line(v, rates, s), cargo_line(v, rates, s), total_line(v, rates, s));
    json!({
        "grade": v.grade.name(),
        "label": v.grade.label(),
        "why": v.why,
        "complaints": v.complaints,
        "price": price,
        "weight": format!("~{} г ({})", v.weight_g.round(), v.weight_basis),
        "cargo": cargo,
        "total": total,
        "banned": v.banned.as_ref().map(|b| format!("⛔ Нельзя авиа: {b}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply() -> Value {
        json!({"grade": "good", "why": "98% хороших отзывов, 2000+ продаж", "complaints": "маломерит",
               "price_cny": 72.6, "weight_g": 850, "weight_basis": "ботинки в коробке", "banned": ""})
    }

    fn rates() -> Rates {
        Rates { uzs_per_cny: 1795.58, uzs_per_usd: 11973.44, date: "13.11.2026".into() }
    }

    #[test]
    fn reads_the_models_verdict() {
        let v = parse_verdict(&reply()).unwrap();
        assert_eq!(v.grade, Grade::Good);
        assert_eq!((v.price_cny, v.weight_g, v.banned.clone()), (Some(72.6), 850.0, None));
        let mut b = reply();
        b["banned"] = json!("магнит в застёжке");
        b["price_cny"] = Value::Null;
        let v = parse_verdict(&b).unwrap();
        assert_eq!((v.price_cny, v.banned.as_deref()), (None, Some("магнит в застёжке")));
    }

    #[test]
    fn the_answer_form_requires_every_field() {
        let s = schema();
        let mut req: Vec<&str> = s["required"].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect();
        req.sort();
        assert_eq!(req, ["banned", "complaints", "grade", "price_cny", "weight_basis", "weight_g", "why"]);
        assert_eq!(s["properties"]["grade"]["enum"], json!(["good", "ok", "bad"]));
        assert_eq!(s["additionalProperties"], false);
    }

    #[test]
    fn zero_weight_or_zero_price_mean_unknown() {
        let mut b = reply();
        b["weight_g"] = json!(0);
        assert!(parse_verdict(&b).is_err());
        let mut b = reply();
        b["price_cny"] = json!(0);
        assert_eq!(parse_verdict(&b).unwrap().price_cny, None);
    }

    #[test]
    fn chinese_shared_by_the_ban_and_the_text_is_listed_once() {
        let mut b = reply();
        b["complaints"] = json!("磁铁 мешает");
        b["banned"] = json!("внутри 磁铁 и 电池");
        assert_eq!(parse_verdict(&b).unwrap().chinese_left(), ["磁铁", "电池"]);
    }

    #[test]
    fn rejects_an_unknown_grade_or_a_silly_weight() {
        let mut b = reply();
        b["grade"] = json!("great");
        assert!(parse_verdict(&b).is_err());
        let mut b = reply();
        b["weight_g"] = json!(-5);
        assert!(parse_verdict(&b).is_err());
    }

    #[test]
    fn chinese_left_in_the_verdict_is_found_and_replaced() {
        let mut b = reply();
        b["complaints"] = json!("尺码偏小 (41), есть запах; 尺码偏小 опять");
        b["why"] = json!("магазин 天猫");
        let mut v = parse_verdict(&b).unwrap();
        assert_eq!(v.chinese_left(), ["天猫", "尺码偏小"]);
        v.replace(&[("尺码偏小".into(), "маломерит".into()), ("天猫".into(), "Tmall".into())]);
        assert_eq!(v.complaints, "маломерит (41), есть запах; маломерит опять");
        assert_eq!(v.why, "магазин Tmall");
        assert!(v.chinese_left().is_empty());
    }

    #[test]
    fn digest_keeps_unique_lines_within_the_limit() {
        let d = digest("  宝贝 \n\n宝贝\n价格 ¥72.6\n\t评价 2000+ \n", 1000);
        assert_eq!(d, "宝贝\n价格 ¥72.6\n评价 2000+");
        assert_eq!(digest("一二三四五\n六七八九十", 7).chars().count(), 7);
    }

    #[test]
    fn card_shows_soums_cargo_and_total() {
        let c = card(&parse_verdict(&reply()).unwrap(), Some(&rates()), &Settings::default());
        assert_eq!(c["grade"], "good");
        assert_eq!(c["label"], "✅ Хорошо");
        assert_eq!(c["price"], "¥72.6 · 130 400 сум");
        assert_eq!(c["weight"], "~850 г (ботинки в коробке)");
        assert_eq!(c["cargo"], "≈ 112 000 сум (Top Cargo авиа, $11/кг)");
        assert_eq!(c["total"], "≈ 242 300 сум с доставкой");
        assert_eq!(c["banned"], Value::Null);
    }

    #[test]
    fn every_grade_has_its_own_label() {
        for (g, label) in [("good", "✅ Хорошо"), ("ok", "🟡 Средне"), ("bad", "❌ Плохо")] {
            let mut r = reply();
            r["grade"] = json!(g);
            let c = card(&parse_verdict(&r).unwrap(), None, &Settings::default());
            assert_eq!((c["grade"].as_str(), c["label"].as_str()), (Some(g), Some(label)));
        }
    }

    #[test]
    fn price_without_rates_is_shown_in_yuan_only() {
        let c = card(&parse_verdict(&reply()).unwrap(), None, &Settings::default());
        assert_eq!(c["price"], "¥72.6");
    }

    #[test]
    fn card_without_rates_or_price_still_shows_the_rest() {
        let mut r = reply();
        r["price_cny"] = Value::Null;
        r["banned"] = json!("аккумулятор");
        let c = card(&parse_verdict(&r).unwrap(), None, &Settings::default());
        assert_eq!(c["price"], Value::Null);
        assert_eq!(c["cargo"], "~$9.35 (Top Cargo авиа, $11/кг)");
        assert_eq!(c["total"], Value::Null);
        assert_eq!(c["banned"], "⛔ Нельзя авиа: аккумулятор");
    }
}
