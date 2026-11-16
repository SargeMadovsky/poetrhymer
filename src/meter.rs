//! Conservative syllabic-tonic fitting, not a pronunciation or semantic model.
use crate::{core, stress};
use serde::{Deserialize, Serialize};
use std::ops::Range;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Meter {
    Iamb,
    Trochee,
    Dactyl,
    Amphibrach,
    Anapest,
}
impl Meter {
    pub const ALL: [Self; 5] = [
        Self::Iamb,
        Self::Trochee,
        Self::Dactyl,
        Self::Amphibrach,
        Self::Anapest,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Iamb => "Ямб",
            Self::Trochee => "Хорей",
            Self::Dactyl => "Дактиль",
            Self::Amphibrach => "Амфибрахий",
            Self::Anapest => "Анапест",
        }
    }
    pub fn pattern(self) -> &'static str {
        match self {
            Self::Iamb => "∪ —",
            Self::Trochee => "— ∪",
            Self::Dactyl => "— ∪ ∪",
            Self::Amphibrach => "∪ — ∪",
            Self::Anapest => "∪ ∪ —",
        }
    }
    fn period(self) -> usize {
        if matches!(self, Self::Iamb | Self::Trochee) {
            2
        } else {
            3
        }
    }
    fn phase(self) -> usize {
        match self {
            Self::Iamb | Self::Amphibrach => 1,
            Self::Anapest => 2,
            _ => 0,
        }
    }
    fn strong(self, i: usize, feet: usize) -> bool {
        i >= self.phase()
            && (i - self.phase()).is_multiple_of(self.period())
            && (i - self.phase()) / self.period() < feet
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Choice {
    #[default]
    Auto,
    Free,
    Regular(Meter),
}
impl Choice {
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "Авто",
            Self::Free => "Свободный / смешанный",
            Self::Regular(m) => m.name(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Scope {
    Line,
    Section,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Override {
    pub scope: Scope,
    pub start: usize,
    /// None identifies the unnamed section before the first heading.
    pub guard: Option<String>,
    pub choice: Choice,
    pub feet: Option<usize>,
}

pub fn anchor(text: &str, line: usize, scope: Scope) -> Option<(usize, Option<String>)> {
    let lines = core::parse_lines(text);
    let selected = lines.get(line)?;
    match scope {
        Scope::Line => (selected.section.is_none() && selected.syllables > 0).then(|| {
            (
                selected.span.start,
                Some(text[selected.span.clone()].to_owned()),
            )
        }),
        Scope::Section => Some(
            lines[..=line]
                .iter()
                .rev()
                .find(|l| l.section.is_some())
                .map_or((0, None), |l| {
                    (l.span.start, Some(text[l.span.clone()].to_owned()))
                }),
        ),
    }
}

pub fn retain_valid(text: &str, overrides: &mut Vec<Override>) {
    let lines = core::parse_lines(text);
    overrides.retain(|item| {
        if item.choice == Choice::Auto || item.feet.is_some_and(|n| !(1..=32).contains(&n)) {
            return false;
        }
        if item.guard.is_none() {
            return item.scope == Scope::Section && item.start == 0;
        }
        lines.iter().any(|line| {
            line.span.start == item.start
                && Some(&text[line.span.clone()]) == item.guard.as_deref()
                && match item.scope {
                    Scope::Line => line.section.is_none() && line.syllables > 0,
                    Scope::Section => line.section.is_some(),
                }
        })
    });
}

/// Keep an override while editing its line, but discard ambiguous split/merge edits.
pub fn rebase(before: &str, after: &str, overrides: &mut Vec<Override>) {
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
        overrides.retain_mut(|item| {
            let Some(guard) = &item.guard else {
                return true;
            };
            let end = item.start + guard.len();
            let inserted = &after[prefix..new_end];
            if old_end <= item.start
                && !(prefix == item.start && old_end == prefix && !inserted.contains('\n'))
            {
                item.start = item.start - old_end + new_end;
            } else if prefix <= end && old_end >= item.start {
                // Enter at the end leaves the existing line intact.
                if prefix == end && old_end == prefix && inserted.starts_with('\n') {
                    return true;
                }
                if prefix < item.start || old_end > end || inserted.contains('\n') {
                    return false;
                }
                let Some(rest) = after.get(item.start..) else {
                    return false;
                };
                item.guard = Some(rest.split('\n').next().unwrap_or("").to_owned());
            }
            true
        });
    }
    retain_valid(after, overrides);
}

pub fn resolve<'a>(text: &str, line: usize, overrides: &'a [Override]) -> Option<&'a Override> {
    for scope in [Scope::Line, Scope::Section] {
        if let Some((start, guard)) = anchor(text, line, scope) {
            if let Some(item) = overrides
                .iter()
                .rev()
                .find(|item| item.scope == scope && item.start == start && item.guard == guard)
            {
                return Some(item);
            }
        }
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Detection {
    Regular(Meter),
    Mixed,
    Ambiguous,
    Insufficient,
}
impl Detection {
    pub fn name(self) -> &'static str {
        match self {
            Self::Regular(m) => m.name(),
            Self::Mixed => "Свободный / смешанный",
            Self::Ambiguous => "Размер неоднозначен",
            Self::Insufficient => "Недостаточно данных",
        }
    }
}

#[derive(Clone, Debug)]
enum WordKind {
    Fixed(Option<usize>),
    Variants(Vec<usize>),
    Flexible,
    Unknown,
}
#[derive(Clone, Debug)]
struct Word {
    range: Range<usize>,
    kind: WordKind,
}

#[derive(Clone, Debug)]
pub struct Line {
    pub index: usize,
    pub start: usize,
    pub section: usize,
    pub vowels: Vec<usize>,
    words: Vec<Word>,
    pub coverage: f32,
    pub anchors: usize,
    pub uncertain_words: usize,
    pub candidates: Vec<Fit>,
    pub detected: Detection,
}

#[derive(Clone, Debug)]
pub struct Fit {
    pub meter: Meter,
    pub feet: usize,
    pub score: f32,
    pub expected: Vec<bool>,
    pub actual: Vec<Option<bool>>,
    pub uncertain: Vec<bool>,
    pub extra: Vec<usize>,
    pub missing: Vec<usize>,
    pub length_mismatch: bool,
}

#[derive(Clone, Debug)]
pub struct Section {
    pub start: usize,
    pub name: String,
    pub lines: Vec<usize>,
    pub detected: Detection,
    pub scores: Vec<(Meter, f32)>,
}
#[derive(Default)]
pub struct Analysis {
    pub lines: Vec<Line>,
    pub sections: Vec<Section>,
}

fn choose(scores: &[(Meter, f32)], coverage: f32, anchors: usize) -> Detection {
    if coverage < 0.55 || anchors < 2 {
        return Detection::Insufficient;
    }
    let Some(&(model, best)) = scores.first() else {
        return Detection::Insufficient;
    };
    if best < 0.82 {
        Detection::Mixed
    } else if scores.get(1).is_some_and(|(_, score)| best - score < 0.07) {
        Detection::Ambiguous
    } else {
        Detection::Regular(model)
    }
}

pub fn fit(line: &Line, meter: Meter, forced_feet: Option<usize>) -> Fit {
    if let Some(feet) = forced_feet {
        return fit_feet(line, meter, feet.clamp(1, 32));
    }
    let len = line.vowels.len();
    let max = ((len.saturating_sub(meter.phase() + 1)) / meter.period() + 1).max(1);
    (max.saturating_sub(1).max(1)..=max)
        .map(|feet| fit_feet(line, meter, feet))
        .filter(|result| {
            let last = meter.phase() + (result.feet - 1) * meter.period();
            last < len && len - 1 - last <= 2
        })
        .max_by(|a, b| a.score.total_cmp(&b.score))
        .unwrap_or_else(|| fit_feet(line, meter, 1))
}

fn fit_feet(line: &Line, meter: Meter, feet: usize) -> Fit {
    let len = line.vowels.len();
    let expected: Vec<_> = (0..len).map(|i| meter.strong(i, feet)).collect();
    let last = meter.phase() + (feet - 1) * meter.period();
    let miss = if meter.period() == 2 { 0.35 } else { 0.8 };
    let mut actual = vec![None; len];
    let mut uncertain = vec![false; len];
    let mut extra = vec![];
    let mut missing = vec![];
    let mut cost = 0.0;
    let mut observed = 0.0;
    for word in &line.words {
        let choice = match &word.kind {
            WordKind::Fixed(value) => *value,
            WordKind::Variants(choices) => choices.iter().copied().min_by(|a, b| {
                let penalty = |v: usize| {
                    word.range
                        .clone()
                        .map(|i| {
                            let stressed = i - word.range.start == v;
                            if stressed && !expected[i] {
                                1.4
                            } else if !stressed && expected[i] {
                                miss
                            } else {
                                0.0
                            }
                        })
                        .sum::<f32>()
                };
                penalty(*a).total_cmp(&penalty(*b))
            }),
            WordKind::Flexible | WordKind::Unknown => {
                for i in word.range.clone() {
                    uncertain[i] = true;
                }
                continue;
            }
        };
        let variant = matches!(word.kind, WordKind::Variants(_));
        if variant {
            cost += 0.15;
        }
        for i in word.range.clone() {
            let stressed = choice == Some(i - word.range.start);
            actual[i] = Some(stressed);
            uncertain[i] = variant;
            if stressed {
                observed += 1.0;
            }
            if stressed && !expected[i] {
                cost += if i == 0 { 0.65 } else { 1.4 };
                extra.push(i);
            }
            if !stressed && expected[i] {
                cost += miss;
                if i == last {
                    cost += 1.0;
                }
                missing.push(i);
            }
        }
    }
    let outside = (0..feet)
        .filter(|n| meter.phase() + n * meter.period() >= len)
        .count();
    let tail = len.saturating_sub(last + 1).saturating_sub(2);
    cost += outside as f32 * 1.4 + tail as f32 * 0.8;
    let denominator = (observed * 1.4 + feet as f32 * miss).max(1.0);
    Fit {
        meter,
        feet,
        score: (1.0 - cost / denominator).clamp(0.0, 1.0),
        expected,
        actual,
        uncertain,
        extra,
        missing,
        length_mismatch: outside > 0 || tail > 0,
    }
}

pub fn analyze(text: &str, stress: &stress::Analysis) -> Analysis {
    let mut out = Analysis::default();
    out.sections.push(Section {
        start: 0,
        name: "Без секции".into(),
        lines: vec![],
        detected: Detection::Insufficient,
        scores: vec![],
    });
    let mut section = 0;
    let mut token_cursor = 0;
    for raw in core::parse_lines(text) {
        if let Some(name) = raw.section {
            section = out.sections.len();
            out.sections.push(Section {
                start: raw.span.start,
                name,
                lines: vec![],
                detected: Detection::Insufficient,
                scores: vec![],
            });
            continue;
        }
        if raw.syllables == 0 {
            continue;
        }
        let vowels: Vec<_> = text[raw.span.clone()]
            .char_indices()
            .filter(|(_, c)| stress::is_vowel(*c))
            .map(|(i, _)| raw.span.start + i)
            .collect();
        let mut line = Line {
            index: raw.index,
            start: raw.span.start,
            section,
            vowels,
            words: vec![],
            coverage: 0.0,
            anchors: 0,
            uncertain_words: 0,
            candidates: vec![],
            detected: Detection::Insufficient,
        };
        while stress
            .tokens
            .get(token_cursor)
            .is_some_and(|t| t.span.end <= raw.span.start)
        {
            token_cursor += 1;
        }
        let mut known = 0.0;
        for token in stress.tokens[token_cursor..]
            .iter()
            .take_while(|t| t.span.start < raw.span.end)
        {
            let positions: Vec<_> = token
                .vowels
                .iter()
                .filter_map(|v| line.vowels.binary_search(&(token.span.start + v)).ok())
                .collect();
            let (Some(&start), Some(&end)) = (positions.first(), positions.last()) else {
                continue;
            };
            let count = positions.len();
            let fixed = matches!(token.source, stress::Source::Manual | stress::Source::Text);
            let kind = if count == 1 && !fixed {
                known += 0.25;
                WordKind::Flexible
            } else if token.source == stress::Source::Unknown {
                line.uncertain_words += 1;
                WordKind::Unknown
            } else if token.source == stress::Source::Ambiguous {
                line.uncertain_words += 1;
                if token.choices.is_empty() {
                    WordKind::Unknown
                } else {
                    known += count as f32 * 0.5;
                    WordKind::Variants(token.choices.clone())
                }
            } else {
                known += count as f32;
                line.anchors += usize::from(token.selected.is_some());
                WordKind::Fixed(token.selected)
            };
            line.words.push(Word {
                range: start..end + 1,
                kind,
            });
        }
        line.coverage = known / line.vowels.len().max(1) as f32;
        line.candidates = Meter::ALL
            .into_iter()
            .map(|model| fit(&line, model, None))
            .collect();
        line.candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
        let scores: Vec<_> = line.candidates.iter().map(|f| (f.meter, f.score)).collect();
        line.detected = choose(&scores, line.coverage, line.anchors);
        out.sections[section].lines.push(out.lines.len());
        out.lines.push(line);
    }
    for group in &mut out.sections {
        if group.lines.is_empty() {
            continue;
        }
        let anchors = group.lines.iter().map(|i| out.lines[*i].anchors).sum();
        let len: usize = group.lines.iter().map(|i| out.lines[*i].vowels.len()).sum();
        let coverage = group
            .lines
            .iter()
            .map(|i| out.lines[*i].coverage * out.lines[*i].vowels.len() as f32)
            .sum::<f32>()
            / len.max(1) as f32;
        group.scores = Meter::ALL
            .into_iter()
            .map(|model| {
                let mut sum = 0.0;
                let mut weights = 0.0;
                for i in &group.lines {
                    let line = &out.lines[*i];
                    if line.coverage < 0.4 || line.anchors == 0 {
                        continue;
                    }
                    let weight = line.anchors as f32;
                    sum += line
                        .candidates
                        .iter()
                        .find(|fit| fit.meter == model)
                        .unwrap()
                        .score
                        * weight;
                    weights += weight;
                }
                (model, if weights > 0.0 { sum / weights } else { 0.0 })
            })
            .collect();
        group.scores.sort_by(|a, b| b.1.total_cmp(&a.1));
        group.detected = choose(&group.scores, coverage, anchors);
        let mut votes = [0usize; 5];
        for i in &group.lines {
            if let Detection::Regular(model) = out.lines[*i].detected {
                votes[Meter::ALL.iter().position(|m| *m == model).unwrap()] += 1;
            }
        }
        let total = votes.iter().sum::<usize>();
        if total >= 2
            && votes
                .iter()
                .filter(|count| **count as f32 / total as f32 >= 0.3)
                .count()
                >= 2
        {
            group.detected = Detection::Mixed;
        }
    }
    out
}

impl Analysis {
    pub fn line(&self, index: usize) -> Option<&Line> {
        self.lines.iter().find(|line| line.index == index)
    }
    pub fn model(
        &self,
        text: &str,
        index: usize,
        overrides: &[Override],
    ) -> (Detection, Option<usize>) {
        if let Some(item) = resolve(text, index, overrides) {
            return (
                match item.choice {
                    Choice::Regular(m) => Detection::Regular(m),
                    Choice::Free => Detection::Mixed,
                    Choice::Auto => Detection::Insufficient,
                },
                item.feet,
            );
        }
        self.line(index)
            .map_or((Detection::Insufficient, None), |line| {
                let section = self.sections[line.section].detected;
                (
                    if matches!(section, Detection::Regular(_)) {
                        section
                    } else {
                        line.detected
                    },
                    None,
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn analyze(text: &str) -> Analysis {
        super::analyze(text, &stress::analyze(text, &[], None))
    }
    #[test]
    fn five_regular_patterns_and_feminine_endings() {
        for (text, model) in [
            ("луна́ луна́ луна́ луна́", Meter::Iamb),
            ("ма́ма ма́ма ма́ма ма́ма", Meter::Trochee),
            ("ра́дуга ра́дуга ра́дуга", Meter::Dactyl),
            ("доро́га доро́га доро́га", Meter::Amphibrach),
            ("тишина́ тишина́ тишина́", Meter::Anapest),
        ] {
            let a = analyze(text);
            assert_eq!(
                a.lines[0].detected,
                Detection::Regular(model),
                "{text}: {:?}",
                a.lines[0].candidates
            );
        }
        let a = analyze("луна́ луна́ луна́ доро́га");
        assert_eq!(a.lines[0].detected, Detection::Regular(Meter::Iamb));
        assert_eq!(a.lines[0].candidates[0].feet, 4);
    }
    #[test]
    fn missing_binary_beat_does_not_force_a_new_meter() {
        let a = analyze("луна́ ма́ма луна́ луна́");
        assert!(
            a.lines[0]
                .candidates
                .iter()
                .find(|f| f.meter == Meter::Iamb)
                .unwrap()
                .extra
                .len()
                > 0
        );
        let a = analyze("луна́ тишина́ луна́ луна́");
        assert!(!a.lines.is_empty());
        let text = "Мой дя́дя са́мых че́стных пра́вил\nКогда́ не в шу́тку занемо́г";
        let a = analyze(text);
        assert_eq!(a.sections[0].detected, Detection::Regular(Meter::Iamb));
    }
    #[test]
    fn unknown_and_all_monosyllables_do_not_claim_certainty() {
        for text in ["фыжобряк крокозябрик", "я ты он мы вы", "за́мок"]
        {
            assert_eq!(analyze(text).lines[0].detected, Detection::Insufficient);
        }
    }

    #[test]
    fn manual_word_stresses_change_detection_without_changing_text() {
        let lexicon = crate::lexicon::Lexicon::from_entries(
            vec![crate::lexicon::Entry {
                word: "мама".into(),
                stress: 0,
                part_of_speech: String::new(),
            }],
            "test".into(),
        );
        let text = "мама мама мама мама";
        let automatic = super::analyze(text, &stress::analyze(text, &[], Some(&lexicon)));
        assert_eq!(
            automatic.lines[0].detected,
            Detection::Regular(Meter::Trochee)
        );
        let marks: Vec<_> = text
            .match_indices("мама")
            .map(|(start, _)| stress::Mark {
                start,
                word: "мама".into(),
                vowel: Some(1),
            })
            .collect();
        let manual = super::analyze(text, &stress::analyze(text, &marks, Some(&lexicon)));
        assert_eq!(manual.lines[0].detected, Detection::Regular(Meter::Iamb));
        assert_eq!(text, "мама мама мама мама");
    }

    #[test]
    fn endings_missing_beats_and_forced_length_are_reported() {
        let a = analyze("ма́ма ма́ма ма́ма ра́дуга");
        assert_eq!(a.lines[0].detected, Detection::Regular(Meter::Trochee));
        assert_eq!(a.lines[0].candidates[0].feet, 4);
        let a = analyze("Когда́ не в шу́тку занемо́г");
        let f = fit(&a.lines[0], Meter::Iamb, None);
        assert_eq!(f.missing, vec![5]);
        assert!(f.extra.is_empty());
        let f = fit(&a.lines[0], Meter::Iamb, Some(6));
        assert!(f.length_mismatch);
        assert!(f.score < 0.82);
    }
    #[test]
    fn mixed_sections_and_headings_are_separate() {
        let a = analyze("[Куплет]\nлуна́ луна́ луна́\nма́ма ма́ма ма́ма\n[Припев]\nтишина́ тишина́ тишина́");
        assert_eq!(a.lines.len(), 3);
        assert_eq!(a.sections[1].detected, Detection::Mixed);
        assert_eq!(a.sections[2].detected, Detection::Regular(Meter::Anapest));
    }
    #[test]
    fn overrides_follow_line_edits_and_resolve_by_scope() {
        let text = "[Куплет]\nлуна луна\nма́ма ма́ма";
        let (start, guard) = anchor(text, 1, Scope::Line).unwrap();
        let mut overrides = vec![
            Override {
                scope: Scope::Section,
                start: 0,
                guard: Some("[Куплет]".into()),
                choice: Choice::Regular(Meter::Trochee),
                feet: None,
            },
            Override {
                scope: Scope::Line,
                start,
                guard,
                choice: Choice::Regular(Meter::Iamb),
                feet: Some(4),
            },
        ];
        assert_eq!(
            resolve(text, 1, &overrides).unwrap().choice,
            Choice::Regular(Meter::Iamb)
        );
        let next = text.replace("луна луна", "светлая луна");
        rebase(text, &next, &mut overrides);
        assert_eq!(overrides.len(), 2);
        assert_eq!(resolve(&next, 1, &overrides).unwrap().feet, Some(4));
        let shifted = format!("Вступление\n{next}");
        rebase(&next, &shifted, &mut overrides);
        assert_eq!(
            resolve(&shifted, 2, &overrides).unwrap().choice,
            Choice::Regular(Meter::Iamb)
        );
    }
}
