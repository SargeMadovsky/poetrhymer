//! Append-only, compressed revisions. Only completed, flushed files are visible.
use std::{
    collections::VecDeque,
    fs::{self, File},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    thread::JoinHandle,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub text: String,
    pub title: Option<String>,
    pub rhyme_links: Vec<crate::core::RhymeLink>,
    #[serde(default)]
    pub stress_marks: Vec<crate::stress::Mark>,
    #[serde(default)]
    pub meter_overrides: Vec<crate::meter::Override>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Summary {
    pub id: String,
    pub document: String,
    pub title: String,
    pub path: Option<PathBuf>,
    pub reason: String,
    pub time: u64,
    pub chars: usize,
    hash: String,
    #[serde(default)]
    text_hash: Option<String>,
}

pub enum Event {
    Saved,
    Error(String),
    Catalogue(Vec<Summary>),
    Preview(String, Snapshot),
}

enum Command {
    Record(String, Snapshot, Option<PathBuf>, String),
    Catalogue,
    Preview(String),
    FileMetadata(PathBuf, String, Sender<Option<Snapshot>>),
    Stop,
}

pub struct History {
    tx: Sender<Command>,
    pub rx: Receiver<Event>,
    worker: Option<JoinHandle<()>>,
    pub pending: usize,
}

pub fn new_document() -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "{}-{}-{}",
        micros(),
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

fn micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

fn header(path: &Path) -> Result<Summary, String> {
    let mut line = String::new();
    BufReader::new(File::open(path).map_err(|e| e.to_string())?)
        .take(1024 * 1024)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    serde_json::from_str(&line).map_err(|e| e.to_string())
}

fn read_revision(root: &Path, id: &str) -> Result<Snapshot, String> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err("Некорректный номер ревизии".into());
    }
    let mut reader =
        BufReader::new(File::open(root.join(format!("{id}.rev"))).map_err(|e| e.to_string())?);
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|e| e.to_string())?;
    let summary: Summary = serde_json::from_str(&line).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    ZlibDecoder::new(reader)
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 64 * 1024 * 1024 || hex::encode(Sha256::digest(&bytes)) != summary.hash {
        return Err("Повреждена ревизия: контрольная сумма не совпадает".into());
    }
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

type Recovery = Option<(Summary, Snapshot)>;

impl History {
    pub fn open(
        root: PathBuf,
        repaint: impl Fn() + Send + 'static,
    ) -> Result<(Self, Recovery, Option<String>), String> {
        fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let mut catalogue = Vec::new();
        let mut warning = None;
        for entry in fs::read_dir(&root).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().is_some_and(|ext| ext == "rev") {
                match header(&path) {
                    Ok(summary)
                        if path.file_stem().and_then(|s| s.to_str())
                            == Some(summary.id.as_str()) =>
                    {
                        catalogue.push(summary)
                    }
                    _ => {
                        warning = Some("Часть истории повреждена; исходные файлы сохранены".into())
                    }
                }
            }
        }
        catalogue.sort_by(|a, b| b.id.cmp(&a.id));
        let mut recovered = None;
        for summary in &catalogue {
            match read_revision(&root, &summary.id) {
                Ok(snapshot) => {
                    recovered = Some((summary.clone(), snapshot));
                    break;
                }
                Err(_) => {
                    warning = Some(
                        "Последняя ревизия повреждена; восстановлена предыдущая доступная версия"
                            .into(),
                    )
                }
            }
        }
        let (tx, commands) = mpsc::channel();
        let (events, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut catalogue: VecDeque<_> = catalogue.into();
            let mut queue = VecDeque::new();
            let mut next = catalogue
                .front()
                .and_then(|s| s.id.parse::<u64>().ok())
                .unwrap_or(0);
            let mut last_error = String::new();
            let mut stopping = false;
            loop {
                match commands.recv_timeout(Duration::from_millis(500)) {
                    Ok(Command::Record(document, snapshot, path, reason)) => {
                        queue.push_back((document, snapshot, path, reason))
                    }
                    Ok(Command::Catalogue) => {
                        let _ = events.send(Event::Catalogue(catalogue.iter().cloned().collect()));
                        repaint();
                    }
                    Ok(Command::Preview(id)) => {
                        let event = match read_revision(&root, &id) {
                            Ok(snapshot) => Event::Preview(id, snapshot),
                            Err(e) => Event::Error(format!("Не удалось прочитать ревизию: {e}")),
                        };
                        let _ = events.send(event);
                        repaint();
                    }
                    Ok(Command::FileMetadata(path, text, reply)) => {
                        let digest = hex::encode(Sha256::digest(text.as_bytes()));
                        let snapshot = catalogue
                            .iter()
                            .filter(|entry| {
                                entry.path.as_ref() == Some(&path)
                                    && entry.text_hash.as_ref() == Some(&digest)
                            })
                            .find_map(|entry| {
                                read_revision(&root, &entry.id)
                                    .ok()
                                    .filter(|snapshot| snapshot.text == text)
                            });
                        let _ = reply.send(snapshot);
                    }
                    Ok(Command::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                        stopping = true
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                while let Some((document, snapshot, path, reason)) = queue.front() {
                    next = micros().max(next + 1);
                    match append(&root, next, document, snapshot, path.clone(), reason) {
                        Ok(summary) => {
                            catalogue.push_front(summary);
                            queue.pop_front();
                            last_error.clear();
                            let _ = events.send(Event::Saved);
                            repaint();
                        }
                        Err(e) => {
                            if e != last_error {
                                let _ = events.send(Event::Error(format!(
                                    "Не удалось записать историю (повторим попытку): {e}"
                                )));
                                repaint();
                                last_error = e;
                            }
                            break;
                        }
                    }
                }
                if stopping {
                    break;
                }
            }
        });
        Ok((
            Self {
                tx,
                rx,
                worker: Some(worker),
                pending: 0,
            },
            recovered,
            warning,
        ))
    }

    pub fn record(
        &mut self,
        document: &str,
        snapshot: Snapshot,
        path: Option<PathBuf>,
        reason: &str,
    ) {
        if self
            .tx
            .send(Command::Record(
                document.into(),
                snapshot,
                path,
                reason.into(),
            ))
            .is_ok()
        {
            self.pending += 1;
        }
    }

    pub fn catalogue(&self) {
        let _ = self.tx.send(Command::Catalogue);
    }
    pub fn preview(&self, id: &str) {
        let _ = self.tx.send(Command::Preview(id.into()));
    }

    pub fn file_metadata(&self, path: PathBuf, text: String) -> Result<Option<Snapshot>, String> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(Command::FileMetadata(path, text, tx))
            .map_err(|e| e.to_string())?;
        rx.recv_timeout(Duration::from_secs(3))
            .map_err(|e| e.to_string())
    }

    pub fn shutdown(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = self.tx.send(Command::Stop);
            let _ = worker.join();
        }
    }
}

impl Drop for History {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn append(
    root: &Path,
    id: u64,
    document: &str,
    snapshot: &Snapshot,
    path: Option<PathBuf>,
    reason: &str,
) -> Result<Summary, String> {
    let bytes = serde_json::to_vec(snapshot).map_err(|e| e.to_string())?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err("Слишком большая ревизия (более 64 МБ)".into());
    }
    let summary = Summary {
        id: format!("{id:020}"),
        document: document.into(),
        title: snapshot
            .title
            .clone()
            .or_else(|| {
                path.as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "Новый текст".into()),
        path,
        reason: reason.into(),
        time: id / 1_000_000,
        chars: snapshot.text.chars().count(),
        hash: hex::encode(Sha256::digest(&bytes)),
        text_hash: Some(hex::encode(Sha256::digest(snapshot.text.as_bytes()))),
    };
    let mut temp = tempfile::NamedTempFile::new_in(root).map_err(|e| e.to_string())?;
    serde_json::to_writer(&mut temp, &summary).map_err(|e| e.to_string())?;
    temp.write_all(b"\n").map_err(|e| e.to_string())?;
    let mut encoder = ZlibEncoder::new(temp, Compression::fast());
    encoder.write_all(&bytes).map_err(|e| e.to_string())?;
    let temp = encoder.finish().map_err(|e| e.to_string())?;
    temp.as_file().sync_all().map_err(|e| e.to_string())?;
    temp.persist_noclobber(root.join(format!("{}.rev", summary.id)))
        .map_err(|e| e.to_string())?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(text: &str) -> Snapshot {
        Snapshot {
            text: text.into(),
            title: Some("Песня".into()),
            rhyme_links: vec![],
            stress_marks: vec![],
            meter_overrides: vec![],
        }
    }

    #[test]
    fn all_edits_survive_restart_and_documents_stay_separate() {
        let dir = tempfile::tempdir().unwrap();
        let (mut history, recovered, _) = History::open(dir.path().into(), || {}).unwrap();
        assert!(recovered.is_none());
        for n in 0..175 {
            history.record("first", snapshot(&format!("Строка {n}")), None, "Набор");
        }
        history.record("second", snapshot("Другая песня"), None, "Новый документ");
        history.shutdown();
        let (mut history, recovered, warning) = History::open(dir.path().into(), || {}).unwrap();
        assert!(warning.is_none());
        let (summary, state) = recovered.unwrap();
        assert_eq!(summary.document, "second");
        assert_eq!(state.text, "Другая песня");
        assert_eq!(state.title.as_deref(), Some("Песня"));
        history.catalogue();
        let Event::Catalogue(entries) = history.rx.recv().unwrap() else {
            panic!()
        };
        assert_eq!(entries.len(), 176);
        let old = read_revision(dir.path(), &entries[175].id).unwrap();
        history.record("first", old, None, "Восстановление");
        history.shutdown();
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 177);
        assert_eq!(
            read_revision(dir.path(), &entries[1].id).unwrap().text,
            "Строка 174"
        );
    }

    #[test]
    fn incomplete_and_corrupt_revisions_do_not_replace_valid_recovery() {
        let dir = tempfile::tempdir().unwrap();
        append(dir.path(), 1, "doc", &snapshot("Сохранено"), None, "Набор").unwrap();
        append(dir.path(), 2, "doc", &snapshot("Повреждено"), None, "Набор").unwrap();
        let path = dir.path().join("00000000000000000002.rev");
        let mut bytes = fs::read(&path).unwrap();
        bytes.truncate(bytes.len() - 8);
        fs::write(path, bytes).unwrap();
        fs::write(dir.path().join("unfinished.tmp"), b"partial write").unwrap();
        let (_history, recovered, warning) = History::open(dir.path().into(), || {}).unwrap();
        assert!(warning.is_some());
        assert_eq!(recovered.unwrap().1.text, "Сохранено");
        assert!(read_revision(dir.path(), "../outside").is_err());
    }

    #[test]
    fn manual_links_and_file_path_survive() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = snapshot("Не спеша\nФуража");
        state
            .rhyme_links
            .push(crate::core::RhymeLink::new(&state.text, 1, Some(0), "спеша".into()).unwrap());
        state.stress_marks.push(crate::stress::Mark {
            start: state.text.find("Фуража").unwrap(),
            word: "Фуража".into(),
            vowel: Some(2),
        });
        let path = PathBuf::from("C:/lyrics/песня.txt");
        let summary = append(
            dir.path(),
            42,
            "doc",
            &state,
            Some(path.clone()),
            "Сохранение",
        )
        .unwrap();
        assert_eq!(
            header(&dir.path().join(format!("{}.rev", summary.id)))
                .unwrap()
                .path,
            Some(path)
        );
        let state = read_revision(dir.path(), &summary.id).unwrap();
        assert!(state.rhyme_links[0].is_valid(&state.text));
        assert_eq!(state.stress_marks.len(), 1);
        assert_eq!(state.stress_marks[0].vowel, Some(2));
    }

    #[test]
    fn reopening_a_file_restores_only_metadata_for_matching_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = PathBuf::from("C:/lyrics/замок.txt");
        let (mut history, _, _) = History::open(dir.path().into(), || {}).unwrap();
        let mut state = snapshot("Замок");
        state.stress_marks.push(crate::stress::Mark {
            start: 0,
            word: "Замок".into(),
            vowel: Some(1),
        });
        state.meter_overrides.push(crate::meter::Override {
            scope: crate::meter::Scope::Line,
            start: 0,
            guard: Some("Замок".into()),
            choice: crate::meter::Choice::Regular(crate::meter::Meter::Iamb),
            feet: Some(2),
        });
        history.record("first", state, Some(path.clone()), "Ручное ударение");
        history.shutdown();
        let (history, _, _) = History::open(dir.path().into(), || {}).unwrap();
        let metadata = history
            .file_metadata(path.clone(), "Замок".into())
            .unwrap()
            .unwrap();
        assert_eq!(metadata.stress_marks[0].vowel, Some(1));
        assert_eq!(metadata.meter_overrides[0].feet, Some(2));
        assert!(history
            .file_metadata(path, "Новый замок".into())
            .unwrap()
            .is_none());
        assert!(history
            .file_metadata(PathBuf::from("C:/other/замок.txt"), "Замок".into())
            .unwrap()
            .is_none());
    }

    #[test]
    fn failed_writes_are_reported_and_retried_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("history");
        let offline = dir.path().join("offline");
        let (mut history, _, _) = History::open(root.clone(), || {}).unwrap();
        fs::rename(&root, &offline).unwrap();
        fs::write(&root, b"not a directory").unwrap();
        history.record("doc", snapshot("Первая правка"), None, "Набор");
        assert!(matches!(
            history.rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            Event::Error(_)
        ));
        history.record("doc", snapshot("Вторая правка"), None, "Набор");
        fs::remove_file(&root).unwrap();
        fs::rename(&offline, &root).unwrap();
        let mut saved = 0;
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while saved < 2 {
            match history
                .rx
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap()
            {
                Event::Saved => saved += 1,
                Event::Error(_) => {} // The directory is briefly absent while it is moved back.
                _ => panic!("unexpected history event"),
            }
        }
        history.shutdown();
        let (_history, recovered, _) = History::open(root.clone(), || {}).unwrap();
        assert_eq!(recovered.unwrap().1.text, "Вторая правка");
        assert_eq!(fs::read_dir(root).unwrap().count(), 2);
    }
}
