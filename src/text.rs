use regex::Regex;
use std::{collections::HashSet, sync::LazyLock};
use unicode_normalization::UnicodeNormalization;

static UUID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").unwrap()
});
static PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
    r"(?i)^[0-9a-f]{8}(?:-[0-9a-f]{0,4})?(?:-[0-9a-f]{0,4})?(?:-[0-9a-f]{0,4})?(?:-[0-9a-f]{0,12})?$").unwrap()
});
static INSTRUCTION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*(# AGENTS\.md\b|<INSTRUCTIONS>)").unwrap());
static TAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
    r"(?i)<(external_[\w-]+|in-app-browser-context|environment_context|permissions_instructions|system-reminder|system_reminder|user_instructions)\b[^>]*>").unwrap()
});
static HAN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\p{Script=Han}").unwrap());
static HAN_PHRASE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\p{Script=Han}{4,64}$").unwrap());
static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[a-z0-9]+").unwrap());

pub fn session_id(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if (trimmed.starts_with("trae-cn://") || trimmed.starts_with("trae://"))
        && let Ok(url) = url::Url::parse(trimmed)
        && matches!(url.scheme(), "trae-cn" | "trae")
        && url.host_str() == Some("session-finder.trae-session-link")
    {
        if url.path() != "/open" {
            return None;
        }
        // An encoded cwd may itself contain a UUID. This URI's id parameter
        // identifies the conversation; generic UUID scanning must not replace it.
        let ids: Vec<_> = url.query_pairs().filter(|(key, _)| key == "id").collect();
        if ids.len() != 1 {
            return None;
        }
        let id = &ids[0].1;
        return (id.len() == 24 && id.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| id.to_ascii_lowercase());
    }
    UUID.find(text)
        .map(|m| m.as_str().to_ascii_lowercase())
        .or_else(|| {
            PREFIX
                .is_match(text.trim())
                .then(|| text.trim().to_ascii_lowercase())
        })
        .or_else(|| {
            let id = text.trim();
            (matches!(id.len(), 8..=24) && id.bytes().all(|b| b.is_ascii_hexdigit()))
                .then(|| id.to_ascii_lowercase())
        })
        .or_else(|| {
            // A turn URI resolves to its conversation, never to a child session.
            let id = text
                .trim()
                .strip_prefix("context://history/")?
                .split('/')
                .next()?;
            (id.len() == 24 && id.bytes().all(|b| b.is_ascii_hexdigit()))
                .then(|| id.to_ascii_lowercase())
        })
}

pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut gap = false;
    for c in text.nfkc().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            out.push(c);
            gap = false;
        } else {
            gap = true;
        }
    }
    out
}

pub fn clean_user(text: &str) -> String {
    if INSTRUCTION.is_match(text) {
        return String::new();
    }
    let mut out = text.to_owned();
    let mut cursor = 0;
    while let Some(capture) = TAG.captures(&out[cursor..]) {
        let open = capture.get(0).unwrap();
        let start = cursor + open.start();
        let after = cursor + open.end();
        let close = format!("</{}>", &capture[1]);
        // ASCII-insensitive matching preserves byte offsets for Unicode text.
        if let Some(end) = out[after..]
            .to_ascii_lowercase()
            .find(&close.to_ascii_lowercase())
        {
            out.replace_range(start..after + end + close.len(), "");
            cursor = start;
        } else {
            cursor = after;
        }
    }
    let out = out.trim();
    out.strip_prefix("<user_input>")
        .and_then(|s| s.strip_suffix("</user_input>"))
        .unwrap_or(out)
        .trim()
        .to_owned()
}

pub fn preview(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(1000)
        .collect()
}

pub fn safe(text: &str, multiline: bool) -> String {
    text.chars()
        .filter(|&c| {
            !(c.is_control() && !(multiline && matches!(c, '\n' | '\t')))
                && !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .collect()
}

pub fn printable(text: &str, limit: usize) -> String {
    let text = safe(text, false)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if text.chars().count() <= limit {
        return text;
    }
    text.chars()
        .take(limit.saturating_sub(1))
        .chain(['…'])
        .collect()
}

fn distance(a: &[u8], b: &[u8], limit: usize) -> usize {
    if a.len().abs_diff(b.len()) > limit {
        return limit + 1;
    }
    let mut older = vec![0; b.len() + 1];
    let mut previous: Vec<_> = (0..=b.len()).collect();
    let mut current = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        current[0] = i;
        for j in 1..=b.len() {
            current[j] = (current[j - 1] + 1)
                .min(previous[j] + 1)
                .min(previous[j - 1] + usize::from(a[i - 1] != b[j - 1]));
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                current[j] = current[j].min(older[j - 2] + 1);
            }
        }
        if *current.iter().min().unwrap() > limit {
            return limit + 1;
        }
        std::mem::swap(&mut older, &mut previous);
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

fn nearby_match(needle: &str, haystack: &str) -> Option<(f64, std::ops::Range<usize>)> {
    let allow_missing = HAN_PHRASE.is_match(needle);
    let needle: Vec<_> = needle.chars().collect();
    if !(2..=64).contains(&needle.len()) {
        return None;
    }
    let offsets: Vec<_> = haystack
        .char_indices()
        .map(|(i, _)| i)
        .chain([haystack.len()])
        .collect();
    let haystack: Vec<_> = haystack.chars().collect();
    let mut best = None;
    for start in 0..haystack.len() {
        if haystack[start] != needle[0] {
            continue;
        }
        let mut at = 1;
        let mut missing = None;
        for (end, &c) in haystack
            .iter()
            .enumerate()
            .skip(start + 1)
            .take(needle.len() * 3 - 1)
        {
            // A longer Han phrase may omit one interior query character.
            // Both endpoints, order and the bounded span must still match.
            // Track this separately so a full subsequence always ranks higher.
            if let Some(next) = missing
                && c == needle[next]
            {
                if next + 1 == needle.len() {
                    best = Some((0.55, offsets[start]..offsets[end + 1]));
                    missing = None;
                } else {
                    missing = Some(next + 1);
                }
            }
            if allow_missing && at + 1 < needle.len() && c == needle[at + 1] {
                if at + 2 == needle.len() {
                    best = Some((0.55, offsets[start]..offsets[end + 1]));
                } else {
                    missing = Some(missing.unwrap_or(0).max(at + 2));
                }
            }
            if c == needle[at] {
                at += 1;
                if at == needle.len() {
                    return Some((0.7, offsets[start]..offsets[end + 1]));
                }
            }
        }
    }
    best
}

/// Locate one normalized query term using the same precedence as search scoring.
pub(crate) fn matching_span(
    term: &str,
    field: &str,
    exact: bool,
) -> Option<(f64, std::ops::Range<usize>)> {
    if let Some(start) = field.find(term) {
        return Some((1.0, start..start + term.len()));
    }
    let compact = field.replace(' ', "");
    let found = if let Some(start) = compact.find(term) {
        Some((0.95, start..start + term.len()))
    } else {
        if exact {
            return None;
        }
        if (4..=48).contains(&term.len())
            && term
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        {
            let limit = if term.len() >= 7 { 2 } else { 1 };
            if let Some(word) = WORD.find_iter(field).find(|w| {
                w.len().abs_diff(term.len()) <= limit
                    && distance(term.as_bytes(), w.as_str().as_bytes(), limit) <= limit
            }) {
                return Some((0.75, word.range()));
            }
        }
        if !HAN.is_match(term) {
            return None;
        }
        nearby_match(term, &compact)
    }?;
    let offsets: Vec<_> = field
        .bytes()
        .enumerate()
        .filter_map(|(i, b)| (b != b' ').then_some(i))
        .collect();
    Some((
        found.0,
        offsets[found.1.start]..offsets[found.1.end - 1] + 1,
    ))
}

/// Fields must already be normalized. Avoid re-normalizing entire cached logs.
pub fn score_normalized(query: &str, fields: &[&str], exact: bool) -> f64 {
    let terms: Vec<_> = query.split_whitespace().collect();
    if terms.is_empty() {
        return 1.0;
    }
    let mut condensed: Option<Vec<String>> = None;
    let mut words: Option<HashSet<&str>> = None;
    let mut sum = 0.0;
    for term in &terms {
        if fields.iter().any(|s| s.contains(term)) {
            sum += 1.0;
            continue;
        }
        let compact =
            condensed.get_or_insert_with(|| fields.iter().map(|v| v.replace(' ', "")).collect());
        if compact.iter().any(|s| s.contains(term)) {
            sum += 0.95;
            continue;
        }
        if exact {
            return 0.0;
        }
        if (4..=48).contains(&term.len())
            && term
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        {
            let words = words.get_or_insert_with(|| {
                fields
                    .iter()
                    .flat_map(|f| WORD.find_iter(f).map(|m| m.as_str()))
                    .collect()
            });
            let limit = if term.len() >= 7 { 2 } else { 1 };
            if words.iter().any(|w| {
                w.len().abs_diff(term.len()) <= limit
                    && distance(term.as_bytes(), w.as_bytes(), limit) <= limit
            }) {
                sum += 0.75;
                continue;
            }
        }
        if HAN.is_match(term) {
            let mut score = 0.0f64;
            for field in compact {
                score = score.max(nearby_match(term, field).map_or(0.0, |m| m.0));
                if score == 0.7 {
                    break;
                }
            }
            if score > 0.0 {
                sum += score;
                continue;
            }
        }
        return 0.0;
    }
    sum / terms.len() as f64
}

pub fn score(query: &str, fields: &[&str], exact: bool) -> f64 {
    let fields: Vec<_> = fields.iter().map(|s| normalize(s)).collect();
    score_normalized(
        &normalize(query),
        &fields.iter().map(String::as_str).collect::<Vec<_>>(),
        exact,
    )
}
