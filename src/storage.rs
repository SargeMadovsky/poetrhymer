use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    pub text: String,
    #[serde(default)]
    pub document_title: Option<String>,
    pub dictionary_folder: String,
    #[serde(default)]
    pub rhyme_links: Vec<crate::core::RhymeLink>,
    #[serde(default)]
    pub last_document_folder: Option<PathBuf>,
    #[serde(default)]
    pub file_path: Option<PathBuf>,
}

/// A recovered draft is already safe in local recovery. For a named file,
/// compare with its actual contents so genuine unsaved edits remain visible.
pub fn saved_text_after_recovery(text: &str, file_path: Option<&Path>) -> Option<String> {
    match file_path {
        Some(path) => read_text(path).ok(),
        None => Some(text.to_owned()),
    }
}

pub fn documents_directory() -> PathBuf {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::{
            System::Com::CoTaskMemFree,
            UI::Shell::{FOLDERID_Documents, SHGetKnownFolderPath},
        };
        let mut value = std::ptr::null_mut();
        if SHGetKnownFolderPath(&FOLDERID_Documents, 0, std::ptr::null_mut(), &mut value) >= 0
            && !value.is_null()
        {
            let mut len = 0;
            while *value.add(len) != 0 {
                len += 1;
            }
            use std::os::windows::ffi::OsStringExt;
            let path = PathBuf::from(std::ffi::OsString::from_wide(std::slice::from_raw_parts(
                value, len,
            )));
            CoTaskMemFree(value.cast());
            return path;
        }
    }
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .map(|path| path.join("Documents"))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn default_dictionary_folder() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(app_directory)
        .join("Rhymes 3")
}

pub fn app_directory() -> PathBuf {
    let executable = std::env::current_exe().ok();
    if let Some(parent) = executable.as_deref().and_then(Path::parent) {
        if matches!(
            parent.file_name().and_then(|name| name.to_str()),
            Some("debug" | "release")
        ) && parent
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "target")
        {
            return parent
                .parent()
                .and_then(Path::parent)
                .unwrap_or(parent)
                .to_owned();
        }
        return parent.to_owned();
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub fn data_directory() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(app_directory)
        .join("Poetrhymer/data")
}

fn recovery_path() -> PathBuf {
    data_directory().join("recovery.json")
}

pub fn load_recovery() -> Option<Session> {
    let path = recovery_path();
    let read = |p: &Path| {
        fs::read(p)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    };
    read(&path)
        .or_else(|| read(&path.with_extension("json.bak")))
        .or_else(|| read(&app_directory().join("local-data/recovery.json")))
}

pub fn save_recovery(session: &Session) -> Result<(), String> {
    let content = serde_json::to_string_pretty(session).map_err(|e| e.to_string())?;
    write_text(&recovery_path(), &content)
}

pub fn read_text(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|e| format!("Не удалось открыть файл: {e}"))?;
    String::from_utf8(bytes)
        .map(|s| s.trim_start_matches('\u{feff}').to_owned())
        .map_err(|_| "Файл должен быть в кодировке UTF-8".into())
}

pub fn write_text(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|e| format!("Не удалось создать папку: {e}"))?;
    }
    // Write and flush a sibling file first. Preserve the previous contents as .bak.
    let mut temp_name = path.as_os_str().to_os_string();
    temp_name.push(".poetrhymer-tmp");
    let temp = PathBuf::from(temp_name);
    let mut backup_name = path.as_os_str().to_os_string();
    backup_name.push(".bak");
    let backup = PathBuf::from(backup_name);
    let save = || -> std::io::Result<()> {
        use std::io::Write;
        let mut file = fs::File::create(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        if path.exists() {
            fs::copy(path, &backup)?;
        }
        // Windows rename replaces an existing file through Rust's MoveFileExW implementation.
        fs::rename(&temp, path)?;
        Ok(())
    };
    save().map_err(|e| format!("Не удалось сохранить файл: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn older_recovery_files_load_without_rhyme_links() {
        let session: Session =
            serde_json::from_str(r#"{"text":"строка","dictionary_folder":"Rhymes 3"}"#).unwrap();
        assert!(session.rhyme_links.is_empty());
        assert!(session.document_title.is_none());
        assert!(session.file_path.is_none());
    }

    #[test]
    fn recovered_named_file_uses_disk_baseline_and_roundtrips_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Песня.txt");
        write_text(&path, "\u{feff}Первая строка\r\nЁж").unwrap();
        let session = Session {
            text: "Первая строка\r\nЁж".into(),
            document_title: Some("Песня".into()),
            dictionary_folder: String::new(),
            rhyme_links: vec![],
            last_document_folder: Some(dir.path().into()),
            file_path: Some(path.clone()),
        };
        let restored: Session =
            serde_json::from_slice(&serde_json::to_vec(&session).unwrap()).unwrap();
        assert_eq!(restored.file_path, Some(path.clone()));
        let baseline = saved_text_after_recovery(&restored.text, restored.file_path.as_deref());
        assert_eq!(baseline.as_deref(), Some(restored.text.as_str()));
        assert_ne!(baseline.as_deref(), Some("Новая правка"));
        assert_eq!(
            saved_text_after_recovery("", Some(&dir.path().join("missing.txt"))),
            None
        );
        assert_eq!(
            saved_text_after_recovery("Автосохранённый черновик", None).as_deref(),
            Some("Автосохранённый черновик")
        );
    }
    #[test]
    fn save_twice_preserves_previous_file_and_utf8() {
        let dir = std::env::temp_dir().join(format!("poetrhymer-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("poem.txt");
        write_text(&path, "Первая строка\r\nЁж").unwrap();
        write_text(&path, "Вторая строка").unwrap();
        assert_eq!(read_text(&path).unwrap(), "Вторая строка");
        assert_eq!(
            read_text(&dir.join("poem.txt.bak")).unwrap(),
            "Первая строка\r\nЁж"
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
