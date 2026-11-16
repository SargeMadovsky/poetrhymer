use crate::lexicon::Lexicon;
use std::{collections::HashSet, ops::Range};

#[derive(Clone, Debug)]
pub struct LineInfo {
    pub index: usize,
    pub span: Range<usize>,
    pub last_word: Option<Range<usize>>,
    /// Some only for standalone [section] headings.
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

pub fn parse_lines(text: &str) -> Vec<LineInfo> {
    let mut offset = 0;
    text.split('\n')
        .enumerate()
        .map(|(index, line)| {
            let trimmed = line.trim();
            let section = trimmed
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .filter(|s| !s.trim().is_empty() && !s.contains(['[', ']']))
                .map(|s| s.trim().to_owned());
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

#[derive(Clone, Debug)]
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

fn explicit_stress(word: &str) -> Option<usize> {
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

fn rhyme_tail(word: &str, stress: usize) -> Option<(String, Option<char>)> {
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
    if limit == 0 {
        return Vec::new();
    }
    let clean = normalized(anchor.trim());
    let stresses: HashSet<usize> = if let Some(stress) = explicit_stress(anchor) {
        [stress].into_iter().collect()
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
    for entry in &lexicon.entries {
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
        let dictionary = Lexicon {
            entries,
            source: "test fixture".into(),
        };
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
