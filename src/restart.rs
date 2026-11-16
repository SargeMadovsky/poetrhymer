//! Two-phase restart: the old editor owns the document until the new GUI is ready.
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct Payload {
    pub session: crate::storage::Session,
    pub document_id: String,
    pub saved_text: Option<String>,
    pub saved_title: Option<String>,
    pub undo: Vec<crate::history::Snapshot>,
    pub redo: Vec<crate::history::Snapshot>,
    pub cursor: Option<usize>,
}

const READY: &str = "POETRHYMER_READY_V1\n";
const MAX_PAYLOAD: usize = 128 * 1024 * 1024;

fn encode(payload: &Payload) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec(payload).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_PAYLOAD {
        return Err("Слишком большой объём данных для перезапуска".into());
    }
    Ok(bytes)
}

fn read_payload(input: &mut impl Read) -> Result<Payload, String> {
    let mut size = [0; 8];
    input.read_exact(&mut size).map_err(|e| e.to_string())?;
    let size = u64::from_le_bytes(size);
    if size > MAX_PAYLOAD as u64 {
        return Err("Недопустимый размер данных перезапуска".into());
    }
    let mut bytes = vec![0; size as usize];
    input.read_exact(&mut bytes).map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

pub struct Incoming {
    pub payload: Payload,
    commit: Receiver<Result<(), String>>,
    notified: bool,
}

impl Incoming {
    pub fn from_arguments() -> Result<Option<Self>, String> {
        if !std::env::args().any(|arg| arg == "--restart-from-stdin") {
            return Ok(None);
        }
        let mut input = std::io::stdin();
        let payload = read_payload(&mut input)?;
        let (tx, commit) = mpsc::channel();
        std::thread::spawn(move || {
            let mut byte = [0];
            let result = input
                .read_exact(&mut byte)
                .map_err(|e| e.to_string())
                .and_then(|()| {
                    if byte == [1] {
                        Ok(())
                    } else {
                        Err("Перезапуск отменён".into())
                    }
                });
            let _ = tx.send(result);
        });
        Ok(Some(Self {
            payload,
            commit,
            notified: false,
        }))
    }

    /// Called only after GUI initialization; editing stays disabled until commit.
    pub fn poll(&mut self) -> Result<bool, String> {
        if !self.notified {
            let mut output = std::io::stdout();
            output
                .write_all(READY.as_bytes())
                .and_then(|()| output.flush())
                .map_err(|e| e.to_string())?;
            self.notified = true;
        }
        match self.commit.try_recv() {
            Ok(result) => result.map(|()| true),
            Err(mpsc::TryRecvError::Empty) => Ok(false),
            Err(_) => Err("Не получено подтверждение перезапуска".into()),
        }
    }
}

pub struct Pending {
    child: Child,
    input: Option<ChildStdin>,
    transfer: Receiver<Result<ChildStdin, String>>,
    ready: Receiver<Result<(), String>>,
    started: Instant,
    committed: bool,
}

impl Pending {
    pub fn poll(&mut self) -> Result<bool, String> {
        if self.started.elapsed() > Duration::from_secs(30) {
            return Err("Новая копия не ответила за 30 секунд".into());
        }
        if self.input.is_none() {
            match self.transfer.try_recv() {
                Ok(result) => self.input = Some(result?),
                Err(mpsc::TryRecvError::Empty) => return Ok(false),
                Err(_) => return Err("Передача текста прервана".into()),
            }
        }
        if let Some(status) = self.child.try_wait().map_err(|e| e.to_string())? {
            return Err(format!("Новая копия завершилась: {status}"));
        }
        match self.ready.try_recv() {
            Ok(result) => {
                result?;
                let input = self.input.as_mut().ok_or("Канал перезапуска закрыт")?;
                input
                    .write_all(&[1])
                    .and_then(|()| input.flush())
                    .map_err(|e| e.to_string())?;
                self.committed = true;
                self.input.take();
                Ok(true)
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("Новая копия не подтвердила запуск".into())
            }
            Err(_) => Ok(false),
        }
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

pub fn prepare(
    executable: &Path,
    payload: Payload,
    checkpoint: Option<Receiver<Result<(), String>>>,
    smoke_test: bool,
) -> Result<Pending, String> {
    let bytes = encode(&payload)?;
    if let Some(checkpoint) = checkpoint {
        checkpoint
            .recv_timeout(Duration::from_secs(15))
            .map_err(|e| format!("История не подтвердила запись: {e}"))??;
    }
    // Recovery is separate from the user's file. Never save that file implicitly.
    if !smoke_test {
        crate::storage::save_recovery(&payload.session)?;
    }
    let mut command = Command::new(executable);
    command
        .arg("--restart-from-stdin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if smoke_test {
        command.args(["--smoke-test", "--smoke-restart-child"]);
    }
    if let Some(parent) = executable.parent() {
        command.current_dir(parent);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let child = command
        .spawn()
        .map_err(|e| format!("Не удалось запустить Poetrhymer: {e}"))?;
    let (tx, ready) = mpsc::channel();
    let (transfer_tx, transfer) = mpsc::channel();
    let mut pending = Pending {
        child,
        input: None,
        transfer,
        ready,
        started: Instant::now(),
        committed: false,
    };
    let mut input = pending
        .child
        .stdin
        .take()
        .ok_or("Нет канала для передачи текста")?;
    // A hung child must not leave the old editor frozen in a blocking pipe write.
    std::thread::spawn(move || {
        let result = input
            .write_all(&(bytes.len() as u64).to_le_bytes())
            .and_then(|()| input.write_all(&bytes))
            .and_then(|()| input.flush())
            .map(|()| input)
            .map_err(|e| e.to_string());
        let _ = transfer_tx.send(result);
    });
    let output = pending
        .child
        .stdout
        .take()
        .ok_or("Нет канала подтверждения")?;
    std::thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(output)
            .take(4096)
            .read_line(&mut line)
            .map_err(|e| e.to_string())
            .and_then(|_| {
                if line == READY {
                    Ok(())
                } else {
                    Err("Новая копия не готова принять текст".into())
                }
            });
        let _ = tx.send(result);
    });
    Ok(pending)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Payload {
        Payload {
            session: serde_json::from_value(serde_json::json!({"text":"Несохранённый стих\n", "dictionary_folder":"", "file_path":"C:/lyrics/песня.txt"})).unwrap(),
            document_id: "same-document".into(), saved_text: Some("Исходник\n".into()),
            saved_title: None, undo: vec![], redo: vec![], cursor: Some(7),
        }
    }

    #[test]
    fn transfer_keeps_dirty_baseline_path_and_unicode() {
        let payload = fixture();
        let bytes = encode(&payload).unwrap();
        let mut framed = (bytes.len() as u64).to_le_bytes().to_vec();
        framed.extend(bytes);
        let restored = read_payload(&mut &framed[..]).unwrap();
        assert_eq!(restored.session.text, payload.session.text);
        assert_eq!(restored.session.file_path, payload.session.file_path);
        assert_eq!(restored.saved_text, payload.saved_text);
        assert_ne!(
            restored.saved_text.as_deref(),
            Some(restored.session.text.as_str())
        );
        assert_eq!(restored.cursor, Some(7));
    }

    #[test]
    fn failed_history_checkpoint_and_missing_executable_abort_restart() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.exe");
        let (tx, rx) = mpsc::channel();
        tx.send(Err("history disk full".into())).unwrap();
        assert_eq!(
            prepare(&missing, fixture(), Some(rx), true).err().unwrap(),
            "history disk full"
        );
        assert!(prepare(&missing, fixture(), None, true)
            .err()
            .unwrap()
            .contains("Не удалось запустить"));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn rejects_truncated_and_oversized_transfer() {
        assert!(read_payload(&mut &[][..]).is_err());
        assert!(read_payload(&mut &(MAX_PAYLOAD as u64 + 1).to_le_bytes()[..]).is_err());
        assert!(read_payload(&mut &10u64.to_le_bytes()[..]).is_err());
    }
}
