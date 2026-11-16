//! Read-only importer for the local Rhymes 3 dictionary container.
//!
//! The prototype imports explicit word lists. Shared-list cache references are
//! counted and omitted until their cache policy has been verified. They must
//! never be interpreted as invented words or presented as a complete import.
use std::{fs, io::Read, path::Path};

use encoding_rs::WINDOWS_1251;
use flate2::read::ZlibDecoder;

#[derive(Clone, Debug)]
pub struct Entry {
    pub word: String,
    /// Zero-based vowel index, not a UTF-8 byte offset.
    pub stress: usize,
    pub part_of_speech: String,
}

#[derive(Debug)]
pub struct Lexicon {
    pub entries: Vec<Entry>,
    pub source: String,
}

const PARTS_OF_SPEECH: [&str; 10] = [
    "существительное",
    "имя собственное",
    "прилагательное",
    "местоимение",
    "глагол",
    "причастие",
    "деепричастие",
    "числительное",
    "наречие",
    "служебное",
];
const MAX_FILE: usize = 128 * 1024 * 1024;
const MAX_EXPANDED: usize = 256 * 1024 * 1024;

impl Lexicon {
    pub fn load_rhymes(folder: &Path) -> Result<Self, String> {
        let path = folder.join("rus_rhymes.dic");
        let size = fs::metadata(&path)
            .map_err(|e| format!("Не удалось открыть {}: {e}", path.display()))?
            .len();
        if size > MAX_FILE as u64 {
            return Err("Словарь слишком большой для этого прототипа".into());
        }
        let bytes = fs::read(&path).map_err(|e| format!("Ошибка чтения словаря: {e}"))?;
        let (prefix_data, clause_data) = unpack_container(&bytes)?;
        let prefixes = read_prefixes(&prefix_data)?;
        let (entries, skipped_lists, _shared_field_sum) = read_clauses(&clause_data, &prefixes)?;
        if entries.is_empty() {
            return Err("В словаре не найдено поддерживаемых словоформ".into());
        }
        Ok(Self {
            source: format!(
                "Rhymes 3 · частичный импорт: {} словоформ; пропущено {} записей с общими списками. Источник: {}",
                entries.len(), skipped_lists, path.display()
            ),
            entries,
        })
    }
}

fn u16_at(data: &[u8], pos: usize) -> Result<u16, String> {
    let bytes = data
        .get(pos..pos + 2)
        .ok_or("Обрезанное 16-битное поле словаря")?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn u32_at(data: &[u8], pos: usize) -> Result<u32, String> {
    let bytes = data
        .get(pos..pos + 4)
        .ok_or("Обрезанное 32-битное поле словаря")?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// The footer describes every compressed or raw block. Its last (20,20)
/// descriptor accounts for the file header, not another block of user data.
fn unpack_container(data: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    if data.len() < 44 || data.get(..4) != Some(&[0x62, 0x1b, 0x93, 0x10]) {
        return Err("Неподдерживаемый формат rus_rhymes.dic (ожидается Rhymes 3)".into());
    }
    if u32_at(data, 4)? as usize != data.len() {
        return Err("Размер файла не совпадает с заголовком Rhymes".into());
    }
    let expected_expanded = u32_at(data, 8)? as usize;
    if expected_expanded > MAX_EXPANDED {
        return Err("Распакованный словарь превышает ограничение прототипа".into());
    }
    let footer_pos = (u32_at(data, 12)? as usize)
        .checked_sub(20)
        .ok_or("Повреждён адрес каталога словаря")?;
    let footer = data.get(footer_pos..).ok_or("Каталог блоков вне файла")?;
    if u32_at(footer, 0)? != 3 {
        return Err("Неизвестная версия каталога блоков Rhymes".into());
    }
    let mut groups = [0usize; 4];
    for (id, count) in groups.iter_mut().enumerate() {
        if u16_at(footer, 4 + id * 4)? as usize != id {
            return Err("Неизвестный порядок секций словаря".into());
        }
        *count = u16_at(footer, 6 + id * 4)? as usize;
    }
    let block_count = u32_at(footer, 20)? as usize;
    if groups[0] != 0
        || groups[1] != 1
        || groups[2] == 0
        || groups[3] == 0
        || block_count != groups.iter().sum::<usize>() + 1
        || footer.len() != 24 + block_count * 8
        || u32_at(footer, 24 + (block_count - 1) * 8)? != 20
        || u32_at(footer, 28 + (block_count - 1) * 8)? != 20
    {
        return Err("Некорректный каталог блоков словаря".into());
    }
    let mut prefix_data = Vec::new();
    let mut clause_data = Vec::new();
    let mut cursor = 20usize;
    let mut expanded_sum = footer.len();
    for block in 0..block_count - 1 {
        let expanded = u32_at(footer, 24 + block * 8)? as usize;
        let compressed = u32_at(footer, 28 + block * 8)? as usize;
        expanded_sum = expanded_sum
            .checked_add(expanded)
            .ok_or("Переполнение размера словаря")?;
        if expanded_sum > expected_expanded || compressed == 0 {
            return Err("Неверные размеры блоков словаря".into());
        }
        let end = cursor
            .checked_add(compressed)
            .ok_or("Переполнение смещения словаря")?;
        if end > footer_pos {
            return Err("Блок словаря пересекает каталог".into());
        }
        let encoded = data.get(cursor..end).ok_or("Обрезанный блок словаря")?;
        let decoded = if expanded == compressed {
            encoded.to_vec()
        } else {
            let decoder = ZlibDecoder::new(encoded);
            let mut out = Vec::with_capacity(expanded);
            decoder
                .take(expanded as u64 + 1)
                .read_to_end(&mut out)
                .map_err(|e| format!("Ошибка сжатого блока {block}: {e}"))?;
            if out.len() != expanded {
                return Err(format!("Неверный размер распакованного блока {block}"));
            }
            out
        };
        if block >= groups[1] && block < groups[1] + groups[2] {
            prefix_data.extend(decoded);
        } else if block >= groups[1] + groups[2] {
            clause_data.extend(decoded);
        }
        cursor = end;
    }
    if cursor != footer_pos || expanded_sum != expected_expanded {
        return Err("Размеры контейнера не прошли проверку".into());
    }
    Ok((prefix_data, clause_data))
}

/// One leading control byte copies a prefix; a second copies a suffix too.
/// Example: автомат-упак + [7,5,'а'] -> автомата-упак.
fn front_string(data: &[u8], cursor: &mut usize, previous: &[u8]) -> Result<Vec<u8>, String> {
    let rest = data.get(*cursor..).ok_or("Строка за пределами словаря")?;
    let len = rest
        .iter()
        .position(|b| *b == 0)
        .ok_or("Незавершённая строка словаря")?;
    let encoded = &rest[..len];
    *cursor += len + 1;
    if encoded.is_empty() {
        return Err("Пустая кодированная строка словаря".into());
    }
    let mut result = Vec::new();
    if encoded[0] < 32 {
        let prefix_len = encoded[0] as usize;
        let (suffix_len, literal_start) = if encoded.get(1).is_some_and(|b| *b < 32) {
            (encoded[1] as usize, 2)
        } else {
            (0, 1)
        };
        if prefix_len > previous.len() || suffix_len > previous.len() {
            return Err("Повреждённая ссылка сжатой строки".into());
        }
        result.extend_from_slice(&previous[..prefix_len]);
        result.extend_from_slice(&encoded[literal_start..]);
        result.extend_from_slice(&previous[previous.len() - suffix_len..]);
    } else {
        result.extend_from_slice(encoded);
    }
    if result.iter().any(|b| *b < 32) {
        return Err("Неизвестное кодирование строки словаря".into());
    }
    Ok(result)
}

fn decode_text(bytes: &[u8]) -> Result<String, String> {
    // These are dictionary fields, not standalone documents. FE FF is the
    // legitimate CP1251 sequence "юя", not a UTF-16 byte-order mark.
    let (value, errors) = WINDOWS_1251.decode_without_bom_handling(bytes);
    if errors {
        return Err("Некорректный Windows-1251 в словаре".into());
    }
    Ok(value.into_owned())
}

fn read_prefixes(data: &[u8]) -> Result<Vec<String>, String> {
    let count = u32_at(data, 4)? as usize;
    if count == 0 || count > 1_000_000 {
        return Err("Неверное число основ словаря".into());
    }
    let mut cursor = 8;
    let mut previous = Vec::new();
    let mut prefixes = Vec::with_capacity(count);
    for _ in 0..count {
        previous = front_string(data, &mut cursor, &previous)?;
        let prefix = decode_text(&previous)?;
        prefixes.push(if prefix == " " { String::new() } else { prefix });
    }
    if cursor != data.len() {
        return Err("Неожиданные данные после таблицы основ".into());
    }
    Ok(prefixes)
}

fn vowel_count(text: &str) -> usize {
    text.chars()
        .filter(|c| "аеёиоуыэюяАЕЁИОУЫЭЮЯ".contains(*c))
        .count()
}

fn read_clauses(data: &[u8], prefixes: &[String]) -> Result<(Vec<Entry>, usize, usize), String> {
    let prefix_stress: Vec<usize> = prefixes.iter().map(|p| vowel_count(p)).collect();
    let mut cursor = 0;
    let mut previous = Vec::new();
    let mut entries = Vec::new();
    let mut skipped_lists = 0;
    let mut skipped_forms = 0;
    while cursor < data.len() {
        previous = front_string(data, &mut cursor, &previous)?;
        let clause = decode_text(&previous)?;
        if vowel_count(&clause) == 0 {
            return Err(format!(
                "Клаузула без ударной гласной: {clause:?}, смещение {cursor}"
            ));
        }
        let mask = u16_at(data, cursor)?;
        let count_flags = u16_at(data, cursor + 2)?;
        cursor += 4;
        if mask == 0 || mask & !0x3ff != 0 {
            return Err("Неизвестные части речи в словаре".into());
        }
        let count = (count_flags & 0x1fff) as usize;
        if count_flags >> 13 != 0 {
            skipped_lists += 1;
            skipped_forms += count;
            continue;
        }
        let width = if mask.count_ones() > 1 { 4 } else { 3 };
        let references = data
            .get(cursor..cursor + count * width)
            .ok_or("Обрезанный список словоформ")?;
        for reference in references.chunks_exact(width) {
            let packed = reference[0] as usize
                | (reference[1] as usize) << 8
                | (reference[2] as usize) << 16;
            let index = packed & 0xfffff;
            let pos = if width == 4 {
                reference[3] as usize
            } else {
                mask.trailing_zeros() as usize
            };
            let prefix = prefixes
                .get(index)
                .ok_or("Ссылка на отсутствующую основу")?;
            if pos >= PARTS_OF_SPEECH.len() || mask & (1 << pos) == 0 {
                return Err("Некорректная часть речи словоформы".into());
            }
            entries.push(Entry {
                word: format!("{prefix}{clause}"),
                stress: prefix_stress[index],
                part_of_speech: PARTS_OF_SPEECH[pos].into(),
            });
        }
        cursor += count * width;
    }
    Ok((entries, skipped_lists, skipped_forms))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn front_coding_copies_both_ends() {
        let (previous, _, _) = WINDOWS_1251.encode("автомат-упак");
        let next = front_string(&[7, 5, 0xe0, 0], &mut 0, &previous).unwrap();
        assert_eq!(decode_text(&next).unwrap(), "автомата-упак");
        assert!(front_string(&[31, b'a', 0], &mut 0, b"short").is_err());
    }

    #[test]
    fn malformed_container_is_rejected() {
        assert!(unpack_container(&[]).is_err());
        assert!(unpack_container(&[0; 100]).is_err());
    }

    #[test]
    fn cp1251_fields_do_not_sniff_unicode_bom() {
        assert_eq!(decode_text(&[0xfe, 0xff]).unwrap(), "юя");
        assert_eq!(decode_text(&[0xff, 0xfe]).unwrap(), "яю");
    }

    #[test]
    #[ignore = "requires the user's local Rhymes installation; no dictionary is distributed"]
    fn local_dictionary_contains_required_forms_and_stresses() {
        let lexicon = Lexicon::load_rhymes(Path::new("Rhymes 3")).unwrap();
        assert!(lexicon.source.contains("частичный импорт"));
        assert!(lexicon.entries.len() > 2_000_000);
        for (word, stress, pos) in [
            ("спеша", 1, "деепричастие"),
            ("фуража", 2, "существительное"),
            ("гаража", 2, "существительное"),
            ("замок", 0, "существительное"),
            ("замок", 1, "существительное"),
        ] {
            assert!(
                lexicon
                    .entries
                    .iter()
                    .any(|e| e.word == word && e.stress == stress && e.part_of_speech == pos),
                "{word}/{stress}/{pos}"
            );
        }
        assert!(lexicon
            .entries
            .iter()
            .all(|e| !e.word.chars().any(char::is_control) && e.stress < vowel_count(&e.word)));
    }
}
