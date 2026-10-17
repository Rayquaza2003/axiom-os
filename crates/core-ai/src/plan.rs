//! Natural language -> structured query. Rules always run (dates stay rule-based: deterministic);
//! with an LLM configured, `refine` lets the model pick terms and tags.
use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime, TimeZone};
use regex::Regex;
use serde_json::Value;

const STOP: &[&str] = &[
    "docs", "doc", "documents", "document", "files", "file", "notes", "note", "my", "the", "all", "a", "an", "of",
    "from", "about", "in", "on", "for", "and", "me", "show", "find", "search", "summarize", "summarise", "summary",
    "what", "is", "are", "how", "does", "do", "with", "this", "last", "to", "give",
];

#[derive(Debug, Default, Clone)]
pub struct Plan {
    pub summarize: bool,
    pub terms: Vec<String>,
    pub tags: Vec<String>,
    pub t0: Option<f64>,
    pub t1: Option<f64>,
    pub confidence: f64,
}

fn ts(d: NaiveDateTime) -> f64 {
    Local.from_local_datetime(&d).earliest().map(|t| t.timestamp() as f64).unwrap_or(0.0)
}

fn midnight(d: NaiveDate) -> NaiveDateTime {
    d.and_hms_opt(0, 0, 0).unwrap()
}

/// Returns (start, end, matched phrase) for a time expression in lowercase `q`.
fn time_span(q: &str, now: f64) -> Option<(f64, f64, String)> {
    let n = Local.timestamp_opt(now as i64, 0).single()?.naive_local();
    let today = n.date();
    let mon = today - Duration::days(today.weekday().num_days_from_monday() as i64);
    let day = |d: NaiveDate| midnight(d);

    if let Some(c) = Regex::new(r"\bq([1-4])(?:\s+(\d{4}))?\b").unwrap().captures(q) {
        let qn: u32 = c[1].parse().ok()?;
        let explicit = c.get(2).and_then(|y| y.as_str().parse::<i32>().ok());
        let mut start = NaiveDate::from_ymd_opt(explicit.unwrap_or(today.year()), 3 * qn - 2, 1)?;
        if explicit.is_none() && start > today {
            start = NaiveDate::from_ymd_opt(today.year() - 1, 3 * qn - 2, 1)?;
        }
        let end = if qn == 4 { NaiveDate::from_ymd_opt(start.year() + 1, 1, 1)? } else { NaiveDate::from_ymd_opt(start.year(), 3 * qn + 1, 1)? };
        return Some((ts(day(start)), ts(day(end)), c[0].to_string()));
    }
    if let Some(c) = Regex::new(r"\blast (\d+) days\b").unwrap().captures(q) {
        let d: i64 = c[1].parse().ok()?;
        return Some((ts(day(today - Duration::days(d))), ts(day(today + Duration::days(1))), c[0].to_string()));
    }
    let first_this = today.with_day(1)?;
    let first_last = (first_this - Duration::days(1)).with_day(1)?;
    let first_next = (first_this + Duration::days(32)).with_day(1)?;
    let spans = [
        ("today", today, today + Duration::days(1)),
        ("yesterday", today - Duration::days(1), today),
        ("this week", mon, mon + Duration::days(7)),
        ("last week", mon - Duration::days(7), mon),
        ("this month", first_this, first_next),
        ("last month", first_last, first_this),
        ("this year", NaiveDate::from_ymd_opt(today.year(), 1, 1)?, NaiveDate::from_ymd_opt(today.year() + 1, 1, 1)?),
    ];
    spans.iter().find(|(p, _, _)| q.contains(p)).map(|(p, a, b)| (ts(day(*a)), ts(day(*b)), p.to_string()))
}

pub fn plan(q: &str, known_tags: &[String], now: f64) -> Plan {
    let ql = q.to_lowercase();
    let span = time_span(&ql, now);
    let mut rest = match &span {
        Some((_, _, phrase)) => ql.replace(phrase.as_str(), " "),
        None => ql.clone(),
    };
    let mut tags = vec![];
    for t in known_tags {
        let re = Regex::new(&format!(r"\b{}s?\b", regex::escape(&t.replace('-', " ")))).unwrap();
        if re.is_match(&rest) {
            rest = re.replace_all(&rest, " ").to_string();
            tags.push(t.clone());
        }
    }
    let terms: Vec<String> = Regex::new(r"[a-z0-9]+").unwrap().find_iter(&rest)
        .map(|m| m.as_str().to_string()).filter(|w| !STOP.contains(&w.as_str())).collect();
    let hits = (!tags.is_empty()) as u8 + span.is_some() as u8;
    let confidence = if terms.is_empty() && hits == 0 { 0.0 } else { (0.6 + 0.2 * hits as f64).min(1.0) };
    Plan {
        summarize: ql.contains("summar"),
        terms,
        tags,
        t0: span.as_ref().map(|s| s.0),
        t1: span.as_ref().map(|s| s.1),
        confidence,
    }
}

/// Prompt asking the LLM to parse `q` into terms and tags (dates are left to the rules).
pub fn llm_prompt(q: &str, known_tags: &[String]) -> String {
    let tags: Vec<&str> = known_tags.iter().take(200).map(String::as_str).collect();
    format!("Turn this search request into JSON: {{\"terms\": [content words to search for], \"tags\": [the known \
             tags the request is about], \"summarize\": true or false}}. Known tags: {}. Ignore dates. Reply with JSON only.\n\
             Request: {q}", tags.join(", "))
}

/// Merge an LLM reply into the rule plan. Unknown tags are dropped; an unparseable reply keeps `base`.
pub fn refine(base: Plan, reply: &str, known_tags: &[String]) -> Plan {
    let json = reply.find('{').zip(reply.rfind('}')).filter(|(a, b)| a < b).map(|(a, b)| &reply[a..=b]);
    let Some(v) = json.and_then(|j| serde_json::from_str::<Value>(j).ok()) else { return base };
    let strs = |k: &str| -> Option<Vec<String>> {
        v[k].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).map(str::to_lowercase).collect())
    };
    let (Some(words), Some(tags)) = (strs("terms"), strs("tags")) else { return base };
    let terms: Vec<String> = words.iter().flat_map(|w| w.split(|c: char| !c.is_alphanumeric()))
        .filter(|w| !w.is_empty() && !STOP.contains(w)).map(String::from).collect();
    let tags: Vec<String> = tags.iter().map(|t| core_data::norm_tag(t)).filter(|t| known_tags.contains(t)).collect();
    let found = !terms.is_empty() || !tags.is_empty() || base.t0.is_some();
    Plan {
        summarize: base.summarize || v["summarize"].as_bool().unwrap_or(false),
        terms,
        tags,
        confidence: if found { 0.9 } else { 0.0 },
        ..base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refine_takes_llm_terms_and_known_tags_only() {
        let known = vec!["finance".to_string(), "travel".to_string()];
        let base = plan("money stuff from q3", &known, 1.79e9);
        assert!(base.tags.is_empty() && base.t0.is_some());
        let p = refine(base.clone(), "Sure:\n{\"terms\": [\"Revenue\", \"the budget\"], \"tags\": [\"Finance\", \"made-up\"], \"summarize\": true}", &known);
        assert_eq!((p.terms.as_slice(), p.tags.as_slice()), (["revenue", "budget"].map(String::from).as_slice(), ["finance".to_string()].as_slice()));
        assert!(p.summarize && p.confidence >= 0.7 && p.t0 == base.t0, "dates come from the rules");
        let kept = refine(base.clone(), "I cannot help with that", &known);
        assert_eq!((kept.terms, kept.confidence), (base.terms, base.confidence));
    }
}
