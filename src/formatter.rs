//! A deliberately small, line-preserving formatter for pasted lyric drafts.
//! It does not interpret list prefixes as markup: dashes may be dialogue.

pub struct FormattedPaste {
    pub text: String,
    pub title: Option<String>,
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let count = line.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&count) || !line[count..].starts_with(char::is_whitespace) {
        return None;
    }
    let mut text = line[count..].trim();
    let without_hashes = text.trim_end_matches('#');
    if without_hashes.ends_with(char::is_whitespace) {
        text = without_hashes.trim_end();
    }
    (!text.is_empty()).then_some((count, text))
}

fn bold_heading(line: &str) -> Option<&str> {
    let line = line.trim_end_matches(':').trim_end();
    for marker in ["***", "___", "**", "__"] {
        if let Some(inner) = line
            .strip_prefix(marker)
            .and_then(|s| s.strip_suffix(marker))
        {
            if !inner.trim().is_empty() && !inner.contains(marker) {
                return Some(inner.trim());
            }
        }
    }
    None
}

fn plain_inline(text: &str) -> String {
    let mut text = text.to_owned();
    for marker in ["***", "___", "**", "__", "*", "_", "`"] {
        let mut out = String::new();
        let mut cursor = 0;
        while let Some(relative) = text[cursor..].find(marker) {
            let start = cursor + relative;
            out.push_str(&text[cursor..start]);
            let body = start + marker.len();
            let valid = text[..start]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric() && c != '\\')
                && text[body..].find(marker).is_some_and(|relative_end| {
                    let end = body + relative_end;
                    end > body
                        && !text[body..end].starts_with(char::is_whitespace)
                        && !text[body..end].ends_with(char::is_whitespace)
                        && text[end + marker.len()..]
                            .chars()
                            .next()
                            .is_none_or(|c| !c.is_alphanumeric())
                });
            if valid {
                let end = body + text[body..].find(marker).unwrap();
                out.push_str(&text[body..end]);
                cursor = end + marker.len();
            } else {
                out.push_str(marker);
                cursor = body;
            }
        }
        out.push_str(&text[cursor..]);
        text = out;
    }
    text
}

fn section(text: &str) -> String {
    let clean = plain_inline(text);
    let clean = clean.trim().trim_end_matches(':').trim_end();
    if clean.starts_with('[') && clean.ends_with(']') {
        clean.to_owned()
    } else {
        format!("[{clean}]")
    }
}

pub fn format_inline(input: &str) -> String {
    plain_inline(input)
}

pub fn format_paste(input: &str, extract_title: bool) -> FormattedPaste {
    let normalized = input.replace("\r\n", "\n").replace('\r', "\n");
    let all_lines: Vec<&str> = normalized.split('\n').collect();
    let first = all_lines.iter().position(|line| !line.trim().is_empty());
    let last = all_lines.iter().rposition(|line| !line.trim().is_empty());
    let lines = match (first, last) {
        (Some(first), Some(last))
            if first < last
                && matches!(
                    all_lines[first].trim(),
                    "```" | "```md" | "```markdown" | "```text" | "```txt" | "```plaintext"
                )
                && all_lines[last].trim() == "```" =>
        {
            &all_lines[first + 1..last]
        }
        _ => &all_lines[..],
    };
    let mut title = None;
    let mut output = Vec::new();
    let mut seen_content = false;
    for line in lines {
        let mut line = line.trim_end();
        while let Some(quoted) = line.strip_prefix("> ") {
            line = quoted;
        }
        let trimmed = line.trim();
        if let Some((level, name)) = heading(trimmed) {
            if extract_title && !seen_content && level == 1 {
                title = Some(plain_inline(name));
                seen_content = true;
                continue;
            }
            output.push(section(name));
        } else if matches!(trimmed, "---" | "***" | "___") {
            output.push(String::new());
        } else if let Some(name) = bold_heading(trimmed) {
            output.push(section(name));
        } else {
            // A single trailing slash is a Markdown hard line break.
            let slash_count = line.chars().rev().take_while(|c| *c == '\\').count();
            if slash_count % 2 == 1 {
                line = &line[..line.len() - 1];
            }
            output.push(plain_inline(line));
        }
        seen_content |= !trimmed.is_empty();
    }
    let text = output.join("\n");
    let text = if title.is_some() {
        text.trim_start_matches('\n').to_owned()
    } else {
        text
    };
    FormattedPaste { text, title }
}

pub fn suggested_filename(title: Option<&str>) -> String {
    let cleaned: String = title
        .unwrap_or("Мой текст")
        .chars()
        .filter(|c| !c.is_control() && !"<>:\"/\\|?*".contains(*c))
        .take(100)
        .collect();
    let stem = cleaned.trim().trim_end_matches(['.', ' ']);
    let stem = if stem.is_empty() {
        "Мой текст"
    } else {
        stem
    };
    let reserved = stem.split('.').next().unwrap_or(stem).to_ascii_uppercase();
    let reserved = matches!(reserved.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (reserved.len() == 4
            && (reserved.starts_with("COM") || reserved.starts_with("LPT"))
            && matches!(reserved.as_bytes()[3], b'1'..=b'9'));
    format!("{}{stem}.txt", if reserved { "_" } else { "" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lyric_draft_becomes_plain_text_with_title_metadata() {
        let draft = "# «Тракторист Семён Петров»\r\n\r\n**Куплет 1**\r\nНад **полями**  \r\nВместе с *нами*\\\r\n\r\n***Припев:***\r\nПой!\r\n\r\n**[Бридж]**\r\n[Пауза.]";
        let result = format_paste(draft, true);
        assert_eq!(result.title.as_deref(), Some("«Тракторист Семён Петров»"));
        assert_eq!(
            result.text,
            "[Куплет 1]\nНад полями\nВместе с нами\n\n[Припев]\nПой!\n\n[Бридж]\n[Пауза.]"
        );
        let second = format_paste(&result.text, true);
        assert_eq!(second.text, result.text);
        assert!(second.title.is_none());
    }

    #[test]
    fn fragments_and_literal_punctuation_are_preserved() {
        let result = format_paste(
            "# Новая секция\n— Не трогай: 2 * 3!\nимя_файла\n  строка с отступом\n[Припев]\n",
            false,
        );
        assert!(result.title.is_none());
        assert_eq!(
            result.text,
            "[Новая секция]\n— Не трогай: 2 * 3!\nимя_файла\n  строка с отступом\n[Припев]\n"
        );
        assert_eq!(format_paste("***", true).text, "");
        assert_eq!(
            format_paste("## Припев\nСлова", true).text,
            "[Припев]\nСлова"
        );
        assert_eq!(
            format_paste("# Песня C#\nТекст", true).title.as_deref(),
            Some("Песня C#")
        );
    }

    #[test]
    fn outer_fences_are_removed_without_collapsing_verse_lines() {
        let result = format_paste(
            "```markdown\n# Песня\n\n__Куплет__\n> Строка\n> Другая строка\n```",
            true,
        );
        assert_eq!(result.title.as_deref(), Some("Песня"));
        assert_eq!(result.text, "[Куплет]\nСтрока\nДругая строка");
        assert_eq!(
            format_paste("обычный текст\nс новой строкой", true).text,
            "обычный текст\nс новой строкой"
        );
    }

    #[test]
    fn title_only_and_section_colons_are_handled() {
        let result = format_paste("# **Название**", true);
        assert_eq!(result.title.as_deref(), Some("Название"));
        assert!(result.text.is_empty());
        assert_eq!(
            format_paste("**Куплет 1**:\nСлова", true).text,
            "[Куплет 1]\nСлова"
        );
    }

    #[test]
    fn inline_paste_keeps_spaces_and_does_not_create_section_tags() {
        assert_eq!(format_inline("**тихими** "), "тихими ");
        assert_eq!(format_inline("*нами*"), "нами");
        assert_eq!(format_inline("имя_файла"), "имя_файла");
    }

    #[test]
    fn filenames_keep_unicode_and_reject_windows_reserved_names() {
        assert_eq!(
            suggested_filename(Some("Песня: «Ночь»?")),
            "Песня «Ночь».txt"
        );
        assert_eq!(suggested_filename(Some("CON")), "_CON.txt");
        assert_eq!(suggested_filename(Some("../")), "Мой текст.txt");
    }
}
