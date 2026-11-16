//! Word stress metadata and analysis; all offsets refer to the original UTF-8 text.
use crate::{core, lexicon::Lexicon};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, ops::Range};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Mark {
    pub start: usize,
    pub word: String,
    /// None deliberately marks a word as unstressed (e.g. a clitic).
    pub vowel: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Source {
    Dictionary,
    Text,
    Manual,
    Single,
    Yo,
    Ambiguous,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub span: Range<usize>,
    pub word: String,
    pub vowels: Vec<usize>,
    pub choices: Vec<usize>,
    pub selected: Option<usize>,
    pub source: Source,
}

#[derive(Default)]
pub struct Analysis {
    pub tokens: Vec<Token>,
    pub ambiguous: usize,
    pub unknown: usize,
}

pub fn is_vowel(c: char) -> bool {
    "аеёиоуыэюяАЕЁИОУЫЭЮЯ".contains(c)
}
/// Visible accent marks are unnecessary on monosyllables and words containing ё.
pub fn needs_mark(word: &str) -> bool {
    core::syllables(word) > 1 && !word.contains(['ё', 'Ё'])
}

pub fn clean(word: &str) -> String {
    word.chars().filter(|c| *c != '\u{301}').collect()
}

fn word_spans(text: &str) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    for line in core::parse_lines(text) {
        if line.section.is_some() {
            continue;
        }
        let mut start = None;
        for (offset, c) in text[line.span.clone()]
            .char_indices()
            .chain(std::iter::once((line.span.len(), ' ')))
        {
            let byte = line.span.start + offset;
            if c.is_alphabetic() || c == '\u{301}' || c == '-' {
                if start.is_none() && c.is_alphabetic() {
                    start = Some(byte);
                }
            } else if let Some(begin) = start.take() {
                let end = text[begin..byte].trim_end_matches('-').len() + begin;
                if text[begin..end].chars().any(is_vowel) {
                    spans.push(begin..end);
                }
            }
        }
    }
    spans
}

pub fn word_at(text: &str, byte: usize) -> Option<Range<usize>> {
    let spans = word_spans(text);
    spans
        .iter()
        .find(|span| span.contains(&byte))
        .or_else(|| spans.iter().find(|span| span.end == byte))
        .cloned()
}

fn dictionary_choices(word: &str, lexicon: Option<&Lexicon>) -> Vec<usize> {
    let Some(lexicon) = lexicon else {
        return vec![];
    };
    let normalized = clean(word).to_lowercase();
    let mut choices = lexicon.stresses(&normalized);
    // Omitted ё is common in pasted lyrics. Only a written ё restricts the spelling.
    if !normalized.contains('ё') {
        for (offset, c) in normalized.char_indices() {
            if c == 'е' {
                let mut variant = normalized.clone();
                variant.replace_range(offset..offset + c.len_utf8(), "ё");
                choices.extend(lexicon.stresses(&variant));
            }
        }
    }
    choices.retain(|stress| *stress < core::syllables(word));
    choices.sort_unstable();
    choices.dedup();
    choices
}

pub fn analyze(text: &str, marks: &[Mark], lexicon: Option<&Lexicon>) -> Analysis {
    let mut cache = HashMap::new();
    let by_start: HashMap<_, _> = marks.iter().map(|mark| (mark.start, mark)).collect();
    let mut analysis = Analysis::default();
    for span in word_spans(text) {
        let word = &text[span.clone()];
        let vowels: Vec<_> = word
            .char_indices()
            .filter(|(_, c)| is_vowel(*c))
            .map(|(i, _)| i)
            .collect();
        let mut explicit = Vec::new();
        let mut last_vowel = None;
        for (offset, c) in word.char_indices() {
            if is_vowel(c) {
                last_vowel = vowels.iter().position(|i| *i == offset);
            } else if c == '\u{301}' {
                if let Some(vowel) = last_vowel.take() {
                    explicit.push(vowel);
                }
            } else {
                last_vowel = None;
            }
        }
        let choices = cache
            .entry(clean(word).to_lowercase())
            .or_insert_with(|| dictionary_choices(word, lexicon))
            .clone();
        let mark = by_start
            .get(&span.start)
            .filter(|mark| mark.word == word && mark.vowel.is_none_or(|v| v < vowels.len()));
        let (selected, source) = if let Some(mark) = mark {
            (mark.vowel, Source::Manual)
        } else if explicit.len() == 1 {
            (Some(explicit[0]), Source::Text)
        } else if explicit.len() > 1 {
            (None, Source::Ambiguous)
        } else if choices.len() == 1 {
            (Some(choices[0]), Source::Dictionary)
        } else if choices.len() > 1 {
            (None, Source::Ambiguous)
        } else if vowels.len() == 1 {
            (Some(0), Source::Single)
        } else if !word.contains('-')
            && word.chars().filter(|c| matches!(c, 'ё' | 'Ё')).count() == 1
        {
            (
                word.char_indices()
                    .find(|(_, c)| matches!(c, 'ё' | 'Ё'))
                    .and_then(|(offset, _)| vowels.iter().position(|v| *v == offset)),
                Source::Yo,
            )
        } else {
            (None, Source::Unknown)
        };
        analysis.ambiguous += usize::from(source == Source::Ambiguous);
        analysis.unknown += usize::from(source == Source::Unknown);
        analysis.tokens.push(Token {
            span,
            word: word.into(),
            vowels,
            choices,
            selected,
            source,
        });
    }
    analysis
}

pub fn retain_valid(text: &str, marks: &mut Vec<Mark>) {
    let spans = word_spans(text);
    marks.retain(|mark| {
        spans
            .binary_search_by_key(&mark.start, |span| span.start)
            .ok()
            .is_some_and(|index| {
                text[spans[index].clone()] == mark.word
                    && mark.vowel.is_none_or(|v| v < core::syllables(&mark.word))
            })
    });
}

/// Shift intact words across one edit; touching a word invalidates its mark.
pub fn rebase(before: &str, after: &str, marks: &mut Vec<Mark>) {
    if before != after {
        let mut prefix = before
            .bytes()
            .zip(after.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        while !before.is_char_boundary(prefix) || !after.is_char_boundary(prefix) {
            prefix -= 1;
        }
        let mut suffix = before[prefix..]
            .bytes()
            .rev()
            .zip(after[prefix..].bytes().rev())
            .take_while(|(a, b)| a == b)
            .count();
        while !before.is_char_boundary(before.len() - suffix)
            || !after.is_char_boundary(after.len() - suffix)
        {
            suffix -= 1;
        }
        let old_end = before.len() - suffix;
        let new_end = after.len() - suffix;
        marks.retain_mut(|mark| {
            if mark.start + mark.word.len() <= prefix {
                true
            } else if mark.start >= old_end {
                mark.start = mark.start - old_end + new_end;
                true
            } else {
                false
            }
        });
    }
    retain_valid(after, marks);
}

pub fn accented(word: &str, vowel: usize) -> String {
    let word = clean(word);
    if word
        .chars()
        .filter(|c| is_vowel(*c))
        .nth(vowel)
        .is_some_and(|c| matches!(c, 'ё' | 'Ё'))
    {
        word
    } else {
        core::with_stress(&word, vowel)
    }
}

pub fn export(text: &str, analysis: &Analysis) -> String {
    let mut out = String::new();
    let mut cursor = 0;
    for token in &analysis.tokens {
        // Also protect callers from stale analysis.
        if token.span.start < cursor || text.get(token.span.clone()) != Some(token.word.as_str()) {
            continue;
        }
        out.push_str(&text[cursor..token.span.start]);
        match token.selected {
            Some(vowel) if needs_mark(&token.word) && !token.word.contains('\u{301}') => {
                out.push_str(&accented(&token.word, vowel))
            }
            _ => out.push_str(&token.word),
        }
        cursor = token.span.end;
    }
    out.push_str(&text[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexicon::Entry;
    fn lexicon() -> Lexicon {
        Lexicon::from_entries(
            vec![("замок", 0), ("замок", 1), ("облачённый", 2), ("ёлочка", 0)]
                .into_iter()
                .map(|(word, stress)| Entry {
                    word: word.into(),
                    stress,
                    part_of_speech: String::new(),
                })
                .collect(),
            "test".into(),
        )
    }
    #[test]
    fn ambiguity_yo_unknown_sections_and_export_preserve_original() {
        let text = "[Куплет 1]\nЗАМОК, облаченный!\nЁлочка и абракадабра.\n";
        let analysis = analyze(text, &[], Some(&lexicon()));
        assert_eq!(analysis.ambiguous, 1);
        assert_eq!(analysis.unknown, 1);
        assert_eq!(analysis.tokens[0].source, Source::Ambiguous);
        assert_eq!(analysis.tokens[1].selected, Some(2));
        assert_eq!(
            export(text, &analysis),
            "[Куплет 1]\nЗАМОК, облаче́нный!\nЁлочка и абракадабра.\n"
        );
        assert!(text.contains("облаченный"));
    }
    #[test]
    fn manual_override_unstressed_and_unicode_acute() {
        let text = "за́мок замок и всё-таки́";
        let marks = vec![
            Mark {
                start: "за́мок ".len(),
                word: "замок".into(),
                vowel: Some(1),
            },
            Mark {
                start: "за́мок замок ".len(),
                word: "и".into(),
                vowel: None,
            },
        ];
        let analysis = analyze(text, &marks, Some(&lexicon()));
        assert_eq!(analysis.tokens[0].source, Source::Text);
        assert_eq!(analysis.tokens[1].source, Source::Manual);
        assert_eq!(export(text, &analysis), "за́мок замо́к и всё-таки́");
        assert_eq!(accented("ЁЛОЧКА", 0), "ЁЛОЧКА");
    }
    #[test]
    fn unrelated_edits_shift_marks_and_touched_words_reset() {
        let original = "[Куплет]\nЗамок\nЗамок";
        let mut marks = vec![Mark {
            start: original.rfind("Замок").unwrap(),
            word: "Замок".into(),
            vowel: Some(1),
        }];
        let shifted = format!("Вступление\n{original}");
        rebase(original, &shifted, &mut marks);
        assert_eq!(marks[0].start, shifted.rfind("Замок").unwrap());
        let changed = format!("{shifted}а");
        rebase(&shifted, &changed, &mut marks);
        assert!(marks.is_empty());
    }
    #[test]
    fn copying_omits_unnecessary_marks_but_keeps_user_written_accents() {
        let text = "Дом и ёлочка, ВСЁ, за́мок, замок и фыжобряк.";
        let analysis = analyze(text, &[], Some(&lexicon()));
        assert_eq!(export(text, &analysis), text);
        assert!(!needs_mark("Дом"));
        assert!(!needs_mark("ёлочка"));
        assert!(!needs_mark("ВСЁ"));
        assert!(needs_mark("за́мок"));
        let manual = analyze(
            "И замок",
            &[Mark {
                start: "И ".len(),
                word: "замок".into(),
                vowel: Some(1),
            }],
            None,
        );
        assert_eq!(export("И замок", &manual), "И замо́к");
    }

    #[test]
    fn malformed_marks_and_heading_marks_are_rejected() {
        let text = "[Замок]\nЗамок";
        let mut marks = vec![
            Mark {
                start: 1,
                word: "Замок".into(),
                vowel: Some(0),
            },
            Mark {
                start: 9,
                word: "Замок".into(),
                vowel: Some(99),
            },
        ];
        retain_valid(text, &mut marks);
        assert!(marks.is_empty());
        let old: crate::history::Snapshot =
            serde_json::from_str(r#"{"text":"Замок","title":null,"rhyme_links":[]}"#).unwrap();
        assert!(old.stress_marks.is_empty());
    }
}
