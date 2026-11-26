use crate::lexicon::Lexicon;
use std::{collections::HashSet, ops::Range};

#[derive(Clone, Debug)]
pub struct LineInfo {
    pub index: usize,
    pub span: Range<usize>,
    pub last_word: Option<Range<usize>>,
    /// Some only for standalone square-bracket or Markdown headings.
    pub section: Option<String>,
    pub syllables: usize,
}

pub fn syllables(text: &str) -> usize {
    text.chars().filter(|c| is_vowel(*c)).count()
}

fn is_vowel(c: char) -> bool {
    "аеёиоуыэюяАЕЁИОУЫЭЮЯ".contains(c)
}

fn is_word_char(c: char) -> bool {
    c.is_alphabetic() || c == '\u{301}' || c == '-'
}

fn heading_title(line: &str) -> Option<String> {
    let line = line.trim();
    let title = if let Some(title) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        (!title.contains(['[', ']'])).then_some(title)?
    } else if let Some(title) = line.strip_prefix("**").and_then(|s| s.strip_suffix("**")) {
        (!title.contains("**")).then_some(title)?
    } else {
        let hashes = line.chars().take_while(|c| *c == '#').count();
        if !(1..=6).contains(&hashes) || !line[hashes..].starts_with(char::is_whitespace) {
            return None;
        }
        line[hashes..].trim().trim_end_matches('#').trim()
    };
    (!title.trim().is_empty()).then(|| title.trim().to_owned())
}

pub fn parse_lines(text: &str) -> Vec<LineInfo> {
    let mut offset = 0;
    text.split('\n')
        .enumerate()
        .map(|(index, line)| {
            let section = heading_title(line);
            let mut end = None;
            let mut start = 0;
            if section.is_none() {
                for (i, c) in line.char_indices().rev() {
                    if end.is_none() {
                        if c.is_alphabetic() || c == '\u{301}' {
                            end = Some(i + c.len_utf8());
                            start = i;
                        }
                    } else if is_word_char(c) {
                        start = i;
                    } else {
                        break;
                    }
                }
            }
            let result = LineInfo {
                index,
                span: offset..offset + line.len(),
                last_word: end.map(|end| offset + start..offset + end),
                syllables: if section.is_some() {
                    0
                } else {
                    syllables(line)
                },
                section,
            };
            offset += line.len() + 1;
            result
        })
        .collect()
}

/// Visual positions in groups of four; absolute line indices remain stable IDs.
pub fn verse_numbers(text: &str) -> Vec<Option<usize>> {
    let mut next = 0;
    parse_lines(text)
        .iter()
        .map(|line| {
            if line.section.is_some() || text[line.span.clone()].trim().is_empty() {
                next = 0;
                None
            } else if line.last_word.is_some() {
                let number = next % 4 + 1;
                next += 1;
                Some(number)
            } else {
                None
            }
        })
        .collect()
}

pub fn line_label(text: &str, index: usize) -> String {
    match verse_numbers(text).get(index).copied().flatten() {
        Some(number) => format!("{number} · строка {}", index + 1),
        None => format!("строка {}", index + 1),
    }
}

#[cfg(test)]
mod numbering_tests {
    #[test]
    fn numbering_restarts_at_stanzas_sections_and_after_four_lines() {
        let text = "[Куплет]\nРаз\nДва\nТри\nЧетыре\nПять\n\nСнова\n[Припев]\nЁж\n";
        assert_eq!(
            super::verse_numbers(text),
            vec![
                None,
                Some(1),
                Some(2),
                Some(3),
                Some(4),
                Some(1),
                None,
                Some(1),
                None,
                Some(1),
                None
            ]
        );
        assert_eq!(super::line_label(text, 9), "1 · строка 10");
        assert_eq!(text.lines().nth(9), Some("Ёж"));
    }
}

/// Suggest a partner only within the current stanza. This lightweight suffix
/// heuristic is a visible suggestion, not an inferred mandatory rhyme scheme.
pub fn suggest_anchor(text: &str, selected: usize) -> Option<usize> {
    let lines = parse_lines(text);
    let line = lines.get(selected)?;
    let target = text.get(line.last_word.clone()?)?;
    let start = (0..selected)
        .rev()
        .find(|i| lines[*i].last_word.is_none())
        .map_or(0, |i| i + 1);
    let end = (selected + 1..lines.len())
        .find(|i| lines[*i].last_word.is_none())
        .unwrap_or(lines.len());
    let ending = |word: &str| {
        normalized(word)
            .chars()
            .filter(|c| *c != 'ь' && *c != 'ъ')
            .map(sound)
            .rev()
            .collect::<Vec<_>>()
    };
    let target_end = ending(target);
    (start..end)
        .filter(|i| *i != selected)
        .filter_map(|i| {
            let word = &text[lines[i].last_word.clone()?];
            let suffix = target_end
                .iter()
                .zip(ending(word))
                .take_while(|(a, b)| **a == *b)
                .count();
            (suffix >= 2).then_some((
                suffix,
                std::cmp::Reverse(i.abs_diff(selected)),
                std::cmp::Reverse(i),
                i,
            ))
        })
        .max()
        .map(|(_, _, _, index)| index)
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RhymeLink {
    pub target: usize,
    pub anchor: Option<usize>,
    pub word: String,
    target_text: String,
    anchor_text: Option<String>,
}

impl RhymeLink {
    pub fn new(text: &str, target: usize, anchor: Option<usize>, word: String) -> Option<Self> {
        let lines = parse_lines(text);
        let target_line = lines.get(target)?;
        target_line.last_word.as_ref()?;
        let anchor_text = match anchor {
            Some(index) => {
                let line = lines.get(index)?;
                line.last_word.as_ref()?;
                Some(text[line.span.clone()].to_owned())
            }
            None => None,
        };
        Some(Self {
            target,
            anchor,
            word,
            target_text: text[target_line.span.clone()].to_owned(),
            anchor_text,
        })
    }

    pub fn is_valid(&self, text: &str) -> bool {
        let lines = parse_lines(text);
        let matches = |index: usize, expected: &str| {
            lines.get(index).is_some_and(|line| {
                line.last_word.is_some() && &text[line.span.clone()] == expected
            })
        };
        matches(self.target, &self.target_text)
            && match (self.anchor, &self.anchor_text) {
                (Some(index), Some(expected)) => index != self.target && matches(index, expected),
                (None, None) => true,
                _ => false,
            }
    }
}

pub fn char_to_byte(text: &str, index: usize) -> usize {
    text.char_indices()
        .nth(index)
        .map_or(text.len(), |(i, _)| i)
}

pub fn byte_to_char(text: &str, index: usize) -> usize {
    text.char_indices().take_while(|(i, _)| *i < index).count()
}

pub fn replace_word(text: &str, span: Range<usize>, replacement: &str) -> Result<String, String> {
    let original = text.get(span.clone()).ok_or("Выделение устарело")?;
    if original.is_empty() || replacement.trim().is_empty() || replacement.contains(['\n', '\r']) {
        return Err("Нельзя заменить слово пустой строкой или переносом".into());
    }
    let replacement = if original
        .chars()
        .filter(|c| c.is_alphabetic())
        .all(char::is_uppercase)
    {
        replacement.to_uppercase()
    } else if original.chars().next().is_some_and(char::is_uppercase) {
        let mut chars = replacement.chars();
        chars
            .next()
            .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
            .unwrap_or_default()
    } else {
        replacement.to_owned()
    };
    let mut updated = text.to_owned();
    updated.replace_range(span, &replacement);
    Ok(updated)
}

#[derive(Clone, Debug, Default)]
pub struct SearchFilter {
    pub syllables: Option<usize>,
    pub part_of_speech: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub word: String,
    pub stressed: String,
    pub syllables: usize,
    pub part_of_speech: String,
    pub score: u32,
}

fn normalized(word: &str) -> String {
    word.chars()
        .filter(|c| *c != '\u{301}')
        .flat_map(char::to_lowercase)
        .collect()
}

pub(crate) fn explicit_stress(word: &str) -> Option<usize> {
    let mut count = 0usize;
    for c in word.chars() {
        if is_vowel(c) {
            count += 1;
        }
        if c == '\u{301}' {
            return count.checked_sub(1);
        }
    }
    None
}

pub fn with_stress(word: &str, stress: usize) -> String {
    let mut result = String::new();
    let mut vowel_index = 0;
    for c in word.chars() {
        result.push(c);
        if is_vowel(c) {
            if vowel_index == stress {
                result.push('\u{301}');
            }
            vowel_index += 1;
        }
    }
    result
}

// A small, explicit approximation for the prototype, not a complete Russian
// pronunciation engine. Softness and full contextual reductions remain future work.
fn sound(c: char) -> char {
    match c {
        'б' => 'п',
        'в' => 'ф',
        'г' => 'к',
        'д' => 'т',
        'ж' => 'ш',
        'з' => 'с',
        _ => c,
    }
}

pub(crate) fn rhyme_tail(word: &str, stress: usize) -> Option<(String, Option<char>)> {
    let mut vowels = 0;
    let mut previous = None;
    for (i, c) in word.char_indices() {
        if is_vowel(c) {
            if vowels == stress {
                return Some((word[i..].chars().map(sound).collect(), previous.map(sound)));
            }
            vowels += 1;
        }
        previous = Some(c);
    }
    None
}

pub fn search_rhymes(
    lexicon: &Lexicon,
    anchor: &str,
    filter: &SearchFilter,
    limit: usize,
) -> Vec<Candidate> {
    search_rhymes_impl(lexicon, anchor, filter, limit, true)
}

fn search_rhymes_impl(
    lexicon: &Lexicon,
    anchor: &str,
    filter: &SearchFilter,
    limit: usize,
    indexed: bool,
) -> Vec<Candidate> {
    if limit == 0 {
        return Vec::new();
    }
    let clean = normalized(anchor.trim());
    let stresses: HashSet<usize> = if let Some(stress) = explicit_stress(anchor) {
        [stress].into_iter().collect()
    } else if indexed {
        lexicon.stresses(&clean).into_iter().collect()
    } else {
        lexicon
            .entries
            .iter()
            .filter(|e| e.word.to_lowercase() == clean)
            .map(|e| e.stress)
            .collect()
    };
    // No invented stress for an unknown word. The UI can ask for an explicit accent.
    let tails: Vec<_> = stresses
        .into_iter()
        .filter_map(|stress| rhyme_tail(&clean, stress))
        .collect();
    if tails.is_empty() {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    let indices = if indexed {
        let mut indices: Vec<usize> = tails
            .iter()
            .flat_map(|(tail, _)| lexicon.rhyming_entries(tail).iter().copied())
            .collect();
        // Original import order preserves duplicate and equal-score tie behavior.
        indices.sort_unstable();
        indices.dedup();
        indices
    } else {
        (0..lexicon.entries.len()).collect()
    };
    for index in indices {
        let entry = &lexicon.entries[index];
        let count = syllables(&entry.word);
        if filter.syllables.is_some_and(|expected| expected != count) {
            continue;
        }
        if filter
            .part_of_speech
            .as_ref()
            .is_some_and(|p| *p != entry.part_of_speech)
        {
            continue;
        }
        let word = entry.word.to_lowercase();
        if word == clean {
            continue;
        }
        let Some((tail, preceding)) = rhyme_tail(&word, entry.stress) else {
            continue;
        };
        let score = tails
            .iter()
            .map(|(anchor_tail, anchor_preceding)| {
                if tail == *anchor_tail {
                    100 + u32::from(preceding == *anchor_preceding) * 20
                } else {
                    0
                }
            })
            .max()
            .unwrap_or(0);
        if score == 0 || !seen.insert((word, entry.stress, entry.part_of_speech.clone())) {
            continue;
        }
        candidates.push(Candidate {
            word: entry.word.clone(),
            stressed: with_stress(&entry.word, entry.stress),
            syllables: count,
            part_of_speech: entry.part_of_speech.clone(),
            score,
        });
    }
    candidates.sort_by_cached_key(|candidate| {
        (
            std::cmp::Reverse(candidate.score),
            candidate.syllables,
            candidate.word.to_lowercase(),
        )
    });
    candidates.truncate(limit);
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexicon::Entry;

    #[test]
    fn indexed_search_matches_full_scan_for_variants_filters_and_limits() {
        let entries = [
            ("спеша", 1, "деепричастие"),
            ("фуража", 2, "существительное"),
            ("Фуража", 2, "существительное"),
            ("багажа", 2, "существительное"),
            ("гаража", 2, "существительное"),
            ("душа", 1, "существительное"),
            ("душа", 0, "деепричастие"),
            ("замок", 0, "существительное"),
            ("замок", 1, "существительное"),
            ("замок", 1, "глагол"),
            ("станок", 1, "существительное"),
            ("рамок", 0, "существительное"),
            ("ёжа", 1, "существительное"),
            ("каша", 0, "существительное"),
        ]
        .into_iter()
        .map(|(word, stress, pos)| Entry {
            word: word.into(),
            stress,
            part_of_speech: pos.into(),
        })
        .collect();
        let lexicon = Lexicon::from_entries(entries, "fixture".into());
        for anchor in [
            "спеша",
            "СПЕША",
            "душа",
            "ду́ша",
            "душа́",
            "замок",
            "ёжа",
            "камыша́",
            "неизвестно",
            "ка́мыша",
            "",
            "x",
        ] {
            for syllables in [None, Some(2), Some(3), Some(9)] {
                for pos in [
                    None,
                    Some("существительное".to_owned()),
                    Some("деепричастие".to_owned()),
                ] {
                    let filter = SearchFilter {
                        syllables,
                        part_of_speech: pos,
                    };
                    for limit in [0, 1, 3, 150] {
                        assert_eq!(
                            search_rhymes(&lexicon, anchor, &filter, limit),
                            search_rhymes_impl(&lexicon, anchor, &filter, limit, false),
                            "{anchor}/{filter:?}/{limit}"
                        );
                    }
                }
            }
        }
        let empty = Lexicon::from_entries(Vec::new(), "empty".into());
        assert!(search_rhymes(&empty, "спеша́", &SearchFilter::default(), 150).is_empty());
    }

    #[test]
    #[ignore = "requires the user's local Rhymes installation; compares old and indexed search"]
    fn local_index_matches_scan_and_reports_timings() {
        let started = std::time::Instant::now();
        let lexicon = Lexicon::load_rhymes(std::path::Path::new("Rhymes 3")).unwrap();
        println!("Import and index: {:?}", started.elapsed());
        for anchor in [
            "спеша",
            "нами",
            "замок",
            "ду́ша",
            "Петров",
            "снег",
            "неизвестно",
            "а́",
        ] {
            let filter = SearchFilter::default();
            let started = std::time::Instant::now();
            let baseline = search_rhymes_impl(&lexicon, anchor, &filter, 150, false);
            let baseline_time = started.elapsed();
            let started = std::time::Instant::now();
            let indexed = search_rhymes(&lexicon, anchor, &filter, 150);
            let indexed_time = started.elapsed();
            println!(
                "{anchor}: scan {baseline_time:?}, index {indexed_time:?}, {} results",
                indexed.len()
            );
            assert_eq!(indexed, baseline, "{anchor}");
        }
    }

    #[test]
    fn markdown_headings_are_not_verse_lines() {
        let text = "# Название\n\n**Куплет 1**\nНад полями\n## Припев\n[Бридж]\n#метка внутри строки\nСлово **внутри** строки";
        let lines = parse_lines(text);
        for index in [0, 2, 4, 5] {
            assert!(lines[index].section.is_some());
            assert!(lines[index].last_word.is_none());
            assert_eq!(lines[index].syllables, 0);
        }
        assert_eq!(lines[2].section.as_deref(), Some("Куплет 1"));
        assert!(lines[6].section.is_none());
        assert!(lines[7].section.is_none());
    }

    #[test]
    fn anchor_stays_in_stanza_and_finds_following_partner() {
        let text = "# Тракторист Семён Петров\n\n**Куплет 1**\nНад полями\nПоднимается туман\nВместе с нами\nНа работу ветеран\n\n[Припев]\nНовая строка";
        assert_eq!(suggest_anchor(text, 3), Some(5));
        assert_eq!(suggest_anchor(text, 5), Some(3));
        assert_eq!(suggest_anchor(text, 4), Some(6));
        assert_eq!(suggest_anchor(text, 9), None);
        assert_eq!(
            suggest_anchor("[Куплет]\nЗаготовил фуража\n\n[Припев]\nНе спеша", 1),
            None
        );
        assert_eq!(
            suggest_anchor("Петрович\nНе спеша\nРаботал\nФуража", 3),
            Some(1)
        );
        assert_eq!(
            suggest_anchor("Петрович\nНе спеша\nРаботал\nФуража", 0),
            None
        );
    }

    #[test]
    fn suggestions_allow_adjacent_alternating_and_enclosed_pairs() {
        for (text, expected) in [
            ("кот\nплот\nлуна\nструна", [1, 0, 3, 2]),
            ("кот\nлуна\nплот\nструна", [2, 3, 0, 1]),
            ("кот\nлуна\nструна\nплот", [3, 2, 1, 0]),
        ] {
            for (index, anchor) in expected.into_iter().enumerate() {
                assert_eq!(suggest_anchor(text, index), Some(anchor));
            }
        }
    }

    #[test]
    fn manual_link_roundtrips_and_rejects_stale_lines() {
        let text = "Над полями\nВместе с нами";
        let link = RhymeLink::new(text, 0, Some(1), "нами".into()).unwrap();
        let json = serde_json::to_string(&link).unwrap();
        let restored: RhymeLink = serde_json::from_str(&json).unwrap();
        assert!(restored.is_valid(text));
        assert!(!restored.is_valid("Над полями\nВместе с вами"));
        assert!(!restored.is_valid("Название\nНад полями\nВместе с нами"));
        assert!(restored.is_valid("Над полями\nВместе с нами\nНовая строка"));
    }

    #[test]
    fn section_and_word_spans_preserve_unicode_and_punctuation() {
        let text = "[Куплет 1]\r\n«Заготовил фуража».\n[Это] внутри строки\n";
        let lines = parse_lines(text);
        assert_eq!(lines[0].section.as_deref(), Some("Куплет 1"));
        assert!(lines[0].last_word.is_none());
        let span = lines[1].last_word.clone().unwrap();
        assert_eq!(&text[span.clone()], "фуража");
        assert_eq!(
            replace_word(text, span, "багажа").unwrap(),
            "[Куплет 1]\r\n«Заготовил багажа».\n[Это] внутри строки\n"
        );
        assert!(lines[2].section.is_none());
        assert!(lines[3].last_word.is_none());
    }

    #[test]
    fn stress_and_hyphen_are_inside_word() {
        let text = "Ёжик, всё-таки́!»";
        let line = &parse_lines(text)[0];
        assert_eq!(&text[line.last_word.clone().unwrap()], "всё-таки́");
        assert_eq!(line.syllables, 5);
        for (byte, _) in text.char_indices() {
            assert_eq!(char_to_byte(text, byte_to_char(text, byte)), byte);
        }
    }

    #[test]
    fn search_uses_anchor_stress_and_target_syllable_filter() {
        let entries = [
            ("спеша", 1),
            ("фуража", 2),
            ("багажа", 2),
            ("душа", 1),
            ("каша", 0),
        ]
        .into_iter()
        .map(|(w, s)| Entry {
            word: w.into(),
            stress: s,
            part_of_speech: "сущ.".into(),
        })
        .collect();
        let dictionary = Lexicon::from_entries(entries, "test fixture".into());
        let results = search_rhymes(
            &dictionary,
            "спеша",
            &SearchFilter {
                syllables: Some(3),
                part_of_speech: None,
            },
            20,
        );
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|c| c.stressed.ends_with("жа́")));
        assert!(search_rhymes(&dictionary, "неизвестно", &SearchFilter::default(), 20).is_empty());
        assert!(
            !search_rhymes(&dictionary, "шалашá", &SearchFilter::default(), 20)
                .iter()
                .any(|c| c.word == "каша")
        );
        assert!(!search_rhymes(&dictionary, "камыша́", &SearchFilter::default(), 20).is_empty());
    }

    #[test]
    fn replacement_preserves_case_and_rejects_invalid_ranges() {
        assert_eq!(replace_word("СЛОВО!", 0..10, "рифма").unwrap(), "РИФМА!");
        assert_eq!(replace_word("Слово!", 0..10, "рифма").unwrap(), "Рифма!");
        assert!(replace_word("ёж", 1..3, "кот").is_err());
    }

    #[test]
    #[ignore = "requires a local Rhymes installation"]
    fn local_dictionary_search_replaces_only_the_target_word() {
        let begin = std::time::Instant::now();
        let dictionary = Lexicon::load_rhymes(std::path::Path::new("Rhymes 3")).unwrap();
        println!("Import: {:?}; {}", begin.elapsed(), dictionary.source);
        let begin = std::time::Instant::now();
        let candidates = search_rhymes(
            &dictionary,
            "спеша",
            &SearchFilter {
                syllables: Some(3),
                part_of_speech: Some("существительное".into()),
            },
            150,
        );
        println!(
            "Search: {:?}; first {:?}",
            begin.elapsed(),
            candidates
                .iter()
                .take(12)
                .map(|c| &c.stressed)
                .collect::<Vec<_>>()
        );
        assert!(candidates
            .iter()
            .any(|candidate| candidate.word == "гаража"));
        let text = "Заготовил фуража».";
        let span = parse_lines(text)[0].last_word.clone().unwrap();
        let replacement = candidates
            .iter()
            .find(|candidate| candidate.word == "гаража")
            .unwrap();
        assert_eq!(
            replace_word(text, span, &replacement.word).unwrap(),
            "Заготовил гаража»."
        );
    }
}
