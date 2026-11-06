use crate::{
    model::{SearchHit, SearchMessage, Session},
    text::{matching_span, normalize, printable, score_normalized},
};
use std::ops::Range;
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

const WIDTH: usize = 180;

// Map normalized bytes back to whole original graphemes, including composed
// accents, fullwidth characters and compatibility ligatures.
fn offsets(original: &str) -> Vec<(usize, usize)> {
    let mut map = Vec::new();
    let mut gap = false;
    for (start, grapheme) in original.grapheme_indices(true) {
        let range = (start, start + grapheme.len());
        for c in grapheme.nfkc().flat_map(char::to_lowercase) {
            if c.is_alphanumeric() {
                if gap && !map.is_empty() {
                    map.push(range);
                }
                map.extend(std::iter::repeat_n(range, c.len_utf8()));
                gap = false;
            } else {
                gap = true;
            }
        }
    }
    map
}

/// Original grapheme ranges for terminal highlighting, using search semantics.
pub(crate) fn highlight_ranges(original: &str, query: &str, exact: bool) -> Vec<Range<usize>> {
    let normalized = normalize(original);
    let query = normalize(query);
    let map = offsets(original);
    let mut ranges: Vec<Range<usize>> = query
        .split_whitespace()
        .filter_map(|term| {
            let (_, span) = matching_span(term, &normalized, exact)?;
            Some(map.get(span.start)?.0..map.get(span.end - 1)?.1)
        })
        .collect();
    ranges.sort_by_key(|r| r.start);
    let mut merged: Vec<Range<usize>> = Vec::new();
    for range in ranges {
        if let Some(previous) = merged.last_mut()
            && range.start <= previous.end
        {
            previous.end = previous.end.max(range.end);
        } else {
            merged.push(range);
        }
    }
    merged
}

fn excerpt(original: &str, span: Range<usize>) -> String {
    let context = (WIDTH - 2)
        .saturating_sub(original[span.clone()].chars().count())
        .min(40);
    let start = if context == 0 {
        span.start
    } else {
        original[..span.start]
            .char_indices()
            .rev()
            .nth(context - 1)
            .map_or(0, |(i, _)| i)
    };
    let end = original[start..]
        .char_indices()
        .nth(WIDTH - 2)
        .map_or(original.len(), |(i, _)| start + i);
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        printable(
            &original[start..end]
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
            WIDTH - 2,
        ),
        if end < original.len() { "…" } else { "" }
    )
}

#[derive(Clone)]
struct Candidate {
    hit: SearchHit,
    scores: Vec<f64>,
}

impl Candidate {
    fn weight(&self, covered: &[bool]) -> f64 {
        self.scores
            .iter()
            .zip(covered)
            .filter(|(_, covered)| !**covered)
            .map(|(score, _)| score)
            .sum()
    }
}

fn collect(
    original: &str,
    field: &str,
    role: Option<&str>,
    terms: &[&str],
    exact: bool,
    best: &mut [Option<Candidate>],
) {
    let normalized = normalize(original);
    let spans: Vec<_> = terms
        .iter()
        .filter_map(|term| matching_span(term, &normalized, exact).map(|m| m.1))
        .collect();
    if spans.is_empty() {
        return;
    }
    let map = offsets(original);
    let uncovered = vec![false; terms.len()];
    for span in spans {
        let (Some(start), Some(end)) = (map.get(span.start), map.get(span.end - 1)) else {
            continue;
        };
        let text = excerpt(original, start.0..end.1);
        let normalized = normalize(&text);
        let scores: Vec<_> = terms
            .iter()
            .map(|term| score_normalized(term, &[&normalized], exact))
            .collect();
        let candidate = Candidate {
            hit: SearchHit {
                field: field.into(),
                role: role.map(str::to_owned),
                text,
            },
            scores,
        };
        for (i, slot) in best.iter_mut().enumerate() {
            if candidate.scores[i] > 0.0
                && slot.as_ref().is_none_or(|old| {
                    candidate.scores[i] > old.scores[i]
                        || (candidate.scores[i] == old.scores[i]
                            && candidate.weight(&uncovered) > old.weight(&uncovered))
                })
            {
                *slot = Some(candidate.clone());
            }
        }
    }
}

/// Only called for the final result page. Original text comes from the search
/// cache, so producing evidence never reopens rollout or database files.
pub fn for_session(
    session: &Session,
    messages: &[SearchMessage],
    query: &str,
    exact: bool,
) -> Vec<SearchHit> {
    let query = normalize(query);
    let mut terms: Vec<_> = query.split_whitespace().collect();
    terms.sort_unstable();
    terms.dedup();
    let mut best = vec![None; terms.len()];
    collect(&session.title, "title", None, &terms, exact, &mut best);
    for message in messages {
        collect(
            &message.text,
            "message",
            Some(&message.role),
            &terms,
            exact,
            &mut best,
        );
    }
    collect(&session.id, "id", None, &terms, exact, &mut best);
    if best.iter().any(Option::is_none) {
        // The original scorer also permits condensed / Han matches spanning
        // adjacent messages. Preserve that behavior and label mixed evidence.
        let joined = messages
            .iter()
            .map(|m| m.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        collect(&joined, "messages", None, &terms, exact, &mut best);
    }
    let mut covered = vec![false; terms.len()];
    let mut hits = Vec::new();
    for _ in 0..2 {
        // Each term retains its first equally useful source.
        let candidate = best
            .iter()
            .flatten()
            .max_by(|a, b| a.weight(&covered).total_cmp(&b.weight(&covered)));
        let Some(candidate) = candidate.filter(|c| c.weight(&covered) > 0.0) else {
            break;
        };
        for (covered, score) in covered.iter_mut().zip(&candidate.scores) {
            *covered |= *score > 0.0;
        }
        hits.push(candidate.hit.clone());
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_unicode_fuzzy_and_separated_terms() {
        for (query, original, expected) in [
            ("café", "前文 Cafe\u{301}，已修复。", "Cafe\u{301}"),
            ("office", "前文 oﬃce，已修复。", "oﬃce"),
            ("abc", "前文 ＡＢＣ，已修复。", "ＡＢＣ"),
            ("modren", "Modern federation: fixed.", "Modern"),
            ("切换主分支", "我想切到主分支，然后继续。", "切到主分支"),
        ] {
            let messages = vec![SearchMessage {
                role: "assistant".into(),
                text: format!("{} {original} {}", "背景 ".repeat(500), "后文 ".repeat(500)),
            }];
            let hits = for_session(&Session::default(), &messages, query, false);
            assert_eq!(hits.len(), 1, "{query}");
            assert!(hits[0].text.contains(expected), "{query}: {:?}", hits[0]);
            assert!(hits[0].text.chars().count() <= WIDTH);
            assert_eq!(hits[0].role.as_deref(), Some("assistant"));
        }
        let messages = vec![
            SearchMessage {
                role: "user".into(),
                text: "Alpha: original request.".into(),
            },
            SearchMessage {
                role: "assistant".into(),
                text: "Zebra: final answer!".into(),
            },
        ];
        let hits = for_session(&Session::default(), &messages, "alpha zebra", true);
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().any(|h| h.text.contains("Alpha")));
        assert!(hits.iter().any(|h| h.text.contains("Zebra")));
        assert!(for_session(&Session::default(), &messages, "alpah", true).is_empty());
    }

    #[test]
    fn condensed_match_can_span_message_boundaries() {
        let messages = vec![
            SearchMessage {
                role: "user".into(),
                text: "Alpha".into(),
            },
            SearchMessage {
                role: "assistant".into(),
                text: "Beta!".into(),
            },
        ];
        let hits = for_session(&Session::default(), &messages, "alphabeta", true);
        assert_eq!(hits[0].field, "messages");
        assert_eq!(hits[0].text, "Alpha Beta!");
        assert!(hits[0].role.is_none());
    }
}
