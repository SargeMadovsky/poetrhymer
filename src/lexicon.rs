//! Read-only importer for the local Rhymes 3 dictionary container.
//!
//! Imports explicit lists and the seven-slot LRU references used by Rhymes 3.
//! The cache is reset at each clause block. Counts, masks and references are
//! validated before reconstructed records are exposed to the editor.
use std::{collections::HashMap, fs, io::Read, path::Path};

use encoding_rs::WINDOWS_1251;
use flate2::read::ZlibDecoder;

pub const RHYMES_FILE: &str = "rus_rhymes.dic";

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
    word_index: Vec<usize>,
    rhyme_index: HashMap<String, Vec<usize>>,
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
const MAX_ENTRIES: usize = 8_000_000;

impl Lexicon {
    /// Indexes contain entry offsets rather than copies of complete words.
    pub fn from_entries(entries: Vec<Entry>, source: String) -> Self {
        let mut word_index: Vec<usize> = (0..entries.len()).collect();
        word_index.sort_by_cached_key(|index| entries[*index].word.to_lowercase());
        let mut rhyme_index: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, entry) in entries.iter().enumerate() {
            if let Some((tail, _)) =
                crate::core::rhyme_tail(&entry.word.to_lowercase(), entry.stress)
            {
                rhyme_index.entry(tail).or_default().push(index);
            }
        }
        Self {
            entries,
            source,
            word_index,
            rhyme_index,
        }
    }

    pub fn stresses(&self, word: &str) -> Vec<usize> {
        let start = self
            .word_index
            .partition_point(|index| self.entries[*index].word.to_lowercase().as_str() < word);
        let mut stresses = Vec::new();
        for index in &self.word_index[start..] {
            let entry = &self.entries[*index];
            if entry.word.to_lowercase() != word {
                break;
            }
            if !stresses.contains(&entry.stress) {
                stresses.push(entry.stress);
            }
        }
        stresses
    }

    pub fn rhyming_entries(&self, tail: &str) -> &[usize] {
        self.rhyme_index.get(tail).map_or(&[], Vec::as_slice)
    }

    pub fn load_rhymes(folder: &Path) -> Result<Self, String> {
        let path = folder.join(RHYMES_FILE);
        let size = fs::metadata(&path)
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    format!("Файл {RHYMES_FILE} не найден в указанной папке.")
                } else {
                    format!("Не удалось открыть {}: {e}", path.display())
                }
            })?
            .len();
        if size > MAX_FILE as u64 {
            return Err("Словарь слишком большой для этого прототипа".into());
        }
        let bytes = fs::read(&path).map_err(|e| format!("Ошибка чтения словаря: {e}"))?;
        let (prefix_data, clause_blocks, expected_forms) = unpack_container(&bytes)?;
        let prefixes = read_prefixes(&prefix_data)?;
        let (entries, shared_lists, clauses) = read_clauses(&clause_blocks, &prefixes)?;
        if entries.len() != expected_forms {
            return Err(format!(
                "Число словоформ не совпало с метаданными: {} вместо {expected_forms}",
                entries.len()
            ));
        }
        if entries.is_empty() {
            return Err("В словаре не найдено поддерживаемых словоформ".into());
        }
        let source = format!(
            "Rhymes 3 · прочитано {} словоформ, {} клаузул и {} общих списков. Источник: {}",
            entries.len(),
            clauses,
            shared_lists,
            path.display()
        );
        Ok(Self::from_entries(entries, source))
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
type UnpackedDictionary = (Vec<u8>, Vec<Vec<u8>>, usize);

fn unpack_container(data: &[u8]) -> Result<UnpackedDictionary, String> {
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
    let mut clause_blocks = Vec::new();
    let mut expected_forms = 0;
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
        if block == 0 {
            if decoded.get(..7) != Some(&[3, 0, 0, 1, 0, 1, 0]) {
                return Err("Неподдерживаемая версия метаданных словаря Rhymes".into());
            }
            expected_forms = u32_at(&decoded, 11)? as usize;
            if expected_forms == 0 || expected_forms > MAX_ENTRIES {
                return Err("Неверное число словоформ в метаданных".into());
            }
        }
        if block >= groups[1] && block < groups[1] + groups[2] {
            prefix_data.extend(decoded);
        } else if block >= groups[1] + groups[2] {
            clause_blocks.push(decoded);
        }
        cursor = end;
    }
    if cursor != footer_pos || expanded_sum != expected_expanded {
        return Err("Размеры контейнера не прошли проверку".into());
    }
    Ok((prefix_data, clause_blocks, expected_forms))
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

#[derive(Clone)]
struct Reference {
    prefix: usize,
    pos: usize,
}

struct CachedList {
    mask: u16,
    references: Vec<Reference>,
}

fn read_clauses(
    blocks: &[Vec<u8>],
    prefixes: &[String],
) -> Result<(Vec<Entry>, usize, usize), String> {
    let prefix_stress: Vec<usize> = prefixes.iter().map(|p| vowel_count(p)).collect();
    let mut entries = Vec::new();
    let mut shared_lists = 0;
    let mut clauses = 0;
    for data in blocks {
        let mut cursor = 0;
        let mut previous = Vec::new();
        let mut cache: [Option<CachedList>; 7] = std::array::from_fn(|_| None);
        // Front is most recent; a new explicit list replaces the back slot.
        let mut recency: Vec<usize> = (0..7).collect();
        while cursor < data.len() {
            previous = front_string(data, &mut cursor, &previous)?;
            let clause = decode_text(&previous)?;
            if vowel_count(&clause) == 0 {
                return Err(format!("Клаузула без ударной гласной: {clause:?}"));
            }
            let mask = u16_at(data, cursor)?;
            let count_flags = u16_at(data, cursor + 2)?;
            cursor += 4;
            if mask == 0 || mask & !0x3ff != 0 {
                return Err("Неизвестные части речи в словаре".into());
            }
            let count = (count_flags & 0x1fff) as usize;
            if count == 0 {
                return Err("Пустой список словоформ".into());
            }
            if entries.len() + count > MAX_ENTRIES {
                return Err("Число словоформ превышает ограничение импортёра".into());
            }
            let shared = (count_flags >> 13) as usize;
            let slot = if shared != 0 {
                let slot = shared - 1;
                let list = cache[slot]
                    .as_ref()
                    .ok_or("Ссылка на незаполненный слот кэша словаря")?;
                if list.mask != mask || list.references.len() != count {
                    return Err("Общий список не совпал по длине или частям речи".into());
                }
                let position = recency
                    .iter()
                    .position(|index| *index == slot)
                    .ok_or("Повреждена очередь кэша")?;
                recency.remove(position);
                shared_lists += 1;
                slot
            } else {
                let width = if mask.count_ones() > 1 { 4 } else { 3 };
                let encoded = data
                    .get(cursor..cursor + count * width)
                    .ok_or("Обрезанный список словоформ")?;
                let mut references = Vec::with_capacity(count);
                for reference in encoded.chunks_exact(width) {
                    let packed = reference[0] as usize
                        | (reference[1] as usize) << 8
                        | (reference[2] as usize) << 16;
                    let prefix = packed & 0xfffff;
                    let pos = if width == 4 {
                        reference[3] as usize
                    } else {
                        mask.trailing_zeros() as usize
                    };
                    if prefix >= prefixes.len() {
                        return Err("Ссылка на отсутствующую основу".into());
                    }
                    if pos >= PARTS_OF_SPEECH.len() || mask & (1 << pos) == 0 {
                        return Err("Некорректная часть речи словоформы".into());
                    }
                    references.push(Reference { prefix, pos });
                }
                cursor += count * width;
                let slot = recency.pop().ok_or("Пустая очередь кэша")?;
                cache[slot] = Some(CachedList { mask, references });
                slot
            };
            recency.insert(0, slot);
            let list = cache[slot].as_ref().ok_or("Слот кэша не заполнен")?;
            for reference in &list.references {
                entries.push(Entry {
                    word: format!("{}{clause}", prefixes[reference.prefix]),
                    stress: prefix_stress[reference.prefix],
                    part_of_speech: PARTS_OF_SPEECH[reference.pos].into(),
                });
            }
            clauses += 1;
        }
    }
    Ok((entries, shared_lists, clauses))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn append_record(
        block: &mut Vec<u8>,
        clause: &str,
        mask: u16,
        count: u16,
        shared_slot: Option<u16>,
        references: &[(usize, usize)],
    ) {
        let (encoded, _, errors) = WINDOWS_1251.encode(clause);
        assert!(!errors);
        block.extend_from_slice(&encoded);
        block.push(0);
        block.extend_from_slice(&mask.to_le_bytes());
        let flags = count | shared_slot.map_or(0, |slot| (slot + 1) << 13);
        block.extend_from_slice(&flags.to_le_bytes());
        if shared_slot.is_none() {
            for (prefix, pos) in references {
                let packed = *prefix as u32 | 8 << 20;
                block.extend_from_slice(&packed.to_le_bytes()[..3]);
                if mask.count_ones() > 1 {
                    block.push(*pos as u8);
                }
            }
        }
    }

    #[test]
    fn shared_lists_preserve_recency_and_mixed_parts_of_speech() {
        let prefixes = vec!["".into(), "к".into(), "т".into()];
        let mut block = Vec::new();
        append_record(&mut block, "от", 1, 1, None, &[(1, 0)]); // slot 6
        append_record(&mut block, "ок", 1, 1, None, &[(2, 0)]); // slot 5
        append_record(&mut block, "отик", 1, 1, Some(6), &[]); // touch slot 6
        for clause in ["а", "ба", "ва", "га", "да", "жа"] {
            append_record(&mut block, clause, 1, 1, None, &[(2, 0)]);
        }
        // Touching slot 6 must protect it from the sixth subsequent insertion.
        append_record(&mut block, "ота", 1, 1, Some(6), &[]);
        let (entries, shared, clauses) = read_clauses(&[block], &prefixes).unwrap();
        assert_eq!(shared, 2);
        assert_eq!(clauses, 10);
        assert_eq!(entries.last().unwrap().word, "кота");
        assert_eq!(entries[2].word, "котик");

        let mut block = Vec::new();
        append_record(&mut block, "от", 17, 2, None, &[(1, 0), (2, 4)]);
        append_record(&mut block, "ота", 17, 2, Some(6), &[]);
        let (entries, shared, _) = read_clauses(&[block], &prefixes).unwrap();
        assert_eq!(shared, 1);
        assert_eq!(entries[2].part_of_speech, "существительное");
        assert_eq!(entries[3].part_of_speech, "глагол");
    }

    #[test]
    fn shared_lists_reject_uninitialized_mismatched_and_cross_block_references() {
        let prefixes = vec!["".into()];
        let mut explicit = Vec::new();
        append_record(&mut explicit, "а", 1, 1, None, &[(0, 0)]);
        let mut shared = Vec::new();
        append_record(&mut shared, "я", 1, 1, Some(6), &[]);
        assert!(read_clauses(&[shared.clone()], &prefixes).is_err());
        assert!(read_clauses(&[explicit.clone(), shared], &prefixes).is_err());
        for (mask, count) in [(1, 2), (2, 1)] {
            let mut block = explicit.clone();
            append_record(&mut block, "я", mask, count, Some(6), &[]);
            assert!(read_clauses(&[block], &prefixes).is_err());
        }
        let mut invalid = Vec::new();
        append_record(&mut invalid, "а", 1, 1, None, &[(99, 0)]);
        assert!(read_clauses(&[invalid], &prefixes).is_err());
    }

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
        let mut exit_stresses = lexicon.stresses("выходит");
        exit_stresses.sort_unstable();
        assert_eq!(exit_stresses, vec![0, 1]);
        assert!(lexicon.source.contains("168634 общих списков"));
        assert_eq!(lexicon.entries.len(), 3_139_098);
        assert!(lexicon.entries.len() > 2_000_000);
        for (word, stress, pos) in [
            ("спеша", 1, "деепричастие"),
            ("фуража", 2, "существительное"),
            ("гаража", 2, "существительное"),
            ("замок", 0, "существительное"),
            ("замок", 1, "существительное"),
            ("Уайльд", 1, "имя собственное"),
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
