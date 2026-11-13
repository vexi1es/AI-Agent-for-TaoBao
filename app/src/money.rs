use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub struct Rates {
    pub uzs_per_cny: f64,
    pub uzs_per_usd: f64,
    pub date: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub markup: f64,
    pub cargo_usd_per_kg: f64,
    pub cargo_name: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self { markup: 1.0, cargo_usd_per_kg: 11.0, cargo_name: "Top Cargo авиа".into() }
    }
}

pub fn parse_cbu(json: &str) -> Result<Rates> {
    let v: Value = serde_json::from_str(json)?;
    let rate = |ccy: &str| -> Option<(f64, String)> {
        let x = v.as_array()?.iter().find(|x| x["Ccy"] == ccy)?;
        let nominal: f64 = x["Nominal"].as_str().unwrap_or("1").parse().ok()?;
        let r: f64 = x["Rate"].as_str()?.parse().ok()?;
        Some((r / nominal, x["Date"].as_str().unwrap_or("").to_string()))
    };
    let (cny, date) = rate("CNY").ok_or_else(|| anyhow!("нет курса юаня"))?;
    let (usd, _) = rate("USD").ok_or_else(|| anyhow!("нет курса доллара"))?;
    Ok(Rates { uzs_per_cny: cny, uzs_per_usd: usd, date })
}

pub fn sum(uzs: f64) -> String {
    let n = ((uzs / 100.0).round() * 100.0).max(0.0) as u64;
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    format!("{out} сум")
}

pub fn price_uzs(cny: f64, r: &Rates, s: &Settings) -> f64 {
    cny * r.uzs_per_cny * s.markup
}

pub fn cargo_uzs(weight_g: f64, r: &Rates, s: &Settings) -> f64 {
    weight_g / 1000.0 * s.cargo_usd_per_kg * r.uzs_per_usd
}

impl Settings {
    pub fn load(data: &Path) -> Self {
        let path = data.join("settings.json");
        let d = Self::default();
        let Some(v) = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) else {
            let _ = std::fs::write(
                &path,
                serde_json::to_string_pretty(&json!({"markup": d.markup, "cargo_usd_per_kg": d.cargo_usd_per_kg, "cargo_name": d.cargo_name}))
                    .unwrap_or_default(),
            );
            return d;
        };
        Self {
            markup: v["markup"].as_f64().filter(|m| *m > 0.0).unwrap_or(d.markup),
            cargo_usd_per_kg: v["cargo_usd_per_kg"].as_f64().filter(|m| *m >= 0.0).unwrap_or(d.cargo_usd_per_kg),
            cargo_name: v["cargo_name"].as_str().map(String::from).unwrap_or(d.cargo_name),
        }
    }
}

const CBU: &str = "https://cbu.uz/ru/arkhiv-kursov-valyut/json/";

impl Rates {
    pub async fn load(data: &Path, http: &reqwest::Client) -> Option<Self> {
        let path = data.join("rates.json");
        let cached = std::fs::read_to_string(&path).ok().and_then(|s| parse_cbu(&s).ok());
        let today = today_tashkent();
        if let Some(r) = &cached {
            if r.date == today {
                return cached;
            }
        }
        match http.get(CBU).send().await {
            Ok(resp) => match resp.text().await {
                Ok(body) if parse_cbu(&body).is_ok() => {
                    let _ = std::fs::write(&path, &body);
                    parse_cbu(&body).ok()
                }
                _ => cached,
            },
            Err(_) => cached,
        }
    }
}

fn today_tashkent() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) + 5 * 3600;
    let (y, m, d) = civil(secs / 86400);
    format!("{d:02}.{m:02}.{y}")
}

pub fn civil(day: u64) -> (i64, u32, u32) {
    let z = day as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CBU: &str = r#"[{"Ccy":"USD","Rate":"11973.44","Date":"13.11.2026","Nominal":"1"},
        {"Ccy":"CNY","Rate":"1795.58","Date":"13.11.2026","Nominal":"1"},
        {"Ccy":"JPY","Rate":"77.1","Date":"13.11.2026","Nominal":"1"}]"#;

    fn rates() -> Rates {
        Rates { uzs_per_cny: 1795.58, uzs_per_usd: 11973.44, date: "13.11.2026".into() }
    }

    #[test]
    fn reads_yuan_and_dollar_from_the_central_bank_list() {
        assert_eq!(parse_cbu(CBU).unwrap(), rates());
        assert!(parse_cbu(r#"[{"Ccy":"USD","Rate":"1","Date":"x"}]"#).is_err(), "no yuan rate means no soum prices");
        assert!(parse_cbu("<html>").is_err());
    }

    #[test]
    fn soums_are_rounded_to_hundreds_and_grouped() {
        assert_eq!(sum(130_356.0), "130 400 сум");
        assert_eq!(sum(949.0), "900 сум");
        assert_eq!(sum(950.0), "1 000 сум");
        assert_eq!(sum(1_234_567.0), "1 234 600 сум");
    }

    #[test]
    fn price_uses_the_buyers_markup() {
        let s = Settings { markup: 1.03, ..Settings::default() };
        assert!((price_uzs(100.0, &rates(), &Settings::default()) - 179_558.0).abs() < 0.01);
        assert!((price_uzs(100.0, &rates(), &s) - 184_944.74).abs() < 0.01);
    }

    #[test]
    fn calendar_day_numbers() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_405), (2025, 11, 13));
        assert_eq!(civil(20_453), (2025, 12, 31));
        assert_eq!(civil(20_454), (2026, 1, 1));
        assert_eq!(civil(11_016), (2000, 2, 29));
    }

    #[test]
    fn settings_file_is_created_with_defaults_and_bad_values_are_ignored() {
        let dir = std::env::temp_dir().join(format!("tbtr_set_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = std::fs::remove_file(dir.join("settings.json"));
        assert_eq!(Settings::load(&dir), Settings::default());
        assert!(dir.join("settings.json").exists());
        std::fs::write(dir.join("settings.json"), r#"{"markup": -2, "cargo_usd_per_kg": 6.5}"#).unwrap();
        let s = Settings::load(&dir);
        assert_eq!((s.markup, s.cargo_usd_per_kg), (1.0, 6.5));
    }

    #[test]
    fn cargo_is_dollars_per_kilo_by_weight() {
        let c = cargo_uzs(350.0, &rates(), &Settings::default());
        assert!((c - 0.35 * 11.0 * 11973.44).abs() < 0.01);
    }
}
