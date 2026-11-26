use std::{
    collections::BTreeSet,
    ops::Range,
    path::PathBuf,
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};

use eframe::egui::{self, Color32, FontId, RichText, Stroke, TextFormat};

use crate::{
    core, formatter, history, history::Snapshot as DocumentState, lexicon::Lexicon, meter, restart,
    storage, stress,
};

const SAMPLE: &str = "[Куплет 1]\n«Где ты был, Семён Петрович?»\nТот ответил не спеша:\n«Поле было. Я работал.\nЗаготовил фуража».\n";
const FORMAT_DRAFT: &str = "# «Тракторист Семён Петров»\n\n**Куплет 1**\nНа рассвете над **полями**  \nИ выходит вместе с *нами*\\\n\n***Припев***\nПой!\n";
const FORMATTED_DRAFT: &str =
    "[Куплет 1]\nНа рассвете над полями\nИ выходит вместе с нами\n\n[Припев]\nПой!\n";
const ACCENT: Color32 = Color32::from_rgb(238, 185, 104);
const MUTED: Color32 = Color32::from_rgb(142, 156, 176);
const INK: Color32 = Color32::from_rgb(222, 230, 241);

struct Selection {
    span: Range<usize>,
    word: String,
    line: usize,
    revision: u64,
}

enum Background {
    Stress(u64, Arc<stress::Analysis>, Arc<meter::Analysis>),
    Dictionary(u64, Result<(Arc<Lexicon>, Vec<String>), String>),
    Search(u64, Vec<core::Candidate>),
    UpdateCheck(Result<Option<crate::updater::Manifest>, String>),
    UpdateInstalled(Result<(), String>),
}

enum PendingAction {
    New,
    Open(PathBuf),
    Close,
    Restart,
    CopyRevision(DocumentState),
}

enum RestartState {
    Preparing(mpsc::Receiver<Result<restart::Pending, String>>),
    Waiting(restart::Pending),
}

pub struct PoetApp {
    history: Option<history::History>,
    _history_temp: Option<tempfile::TempDir>,
    document_id: String,
    history_error: Option<String>,
    show_history: bool,
    history_all: bool,
    history_limit: usize,
    history_entries: Vec<history::Summary>,
    history_selected: Option<String>,
    history_preview: Option<DocumentState>,
    meter_mode: bool,
    meter_scope: meter::Scope,
    meter_line: Option<usize>,
    meter_overrides: Vec<meter::Override>,
    meter_analysis: Option<Arc<meter::Analysis>>,
    stress_mode: bool,
    stress_marks: Vec<stress::Mark>,
    stress_text: String,
    stress_analysis: Option<Arc<stress::Analysis>>,
    stress_ticket: u64,
    stress_due: Option<Instant>,
    stress_loading: bool,
    stress_selected: Option<usize>,
    stress_popup_pos: egui::Pos2,
    poem_editor_id: Option<egui::Id>,
    stress_focus: bool,
    text: String,
    saved_text: Option<String>,
    saved_title: Option<String>,
    document_title: Option<String>,
    file_path: Option<PathBuf>,
    last_document_folder: Option<PathBuf>,
    revision: u64,
    undo: Vec<DocumentState>,
    redo: Vec<DocumentState>,
    last_typing: Option<Instant>,
    selection: Option<Selection>,
    anchor: String,
    anchor_line: Option<usize>,
    rhyme_links: Vec<core::RhymeLink>,
    match_syllables: bool,
    closer_only: bool,
    compact_rhymes: bool,
    reveal_candidate: bool,
    syllable_count: usize,
    part_of_speech: String,
    candidates: Vec<core::Candidate>,
    chosen: Option<usize>,
    lexicon: Option<Arc<Lexicon>>,
    parts_of_speech: Vec<String>,
    dictionary_folder: String,
    dictionary_error: Option<String>,
    dictionary_loading: bool,
    dictionary_ticket: u64,
    show_dictionary: bool,
    search_ticket: u64,
    search_loading: bool,
    search_due: Option<Instant>,
    tx: mpsc::Sender<Background>,
    rx: mpsc::Receiver<Background>,
    status: String,
    status_is_error: bool,
    autosave_due: Option<Instant>,
    pending_action: Option<PendingAction>,
    allow_close: bool,
    requested_cursor: Option<usize>,
    requested_scroll: Option<usize>,
    editor_scroll: f32,
    visible_section: Option<usize>,
    restart_available: bool,
    restart_executable: Result<PathBuf, String>,
    restart_state: Option<RestartState>,
    restart_committed: bool,
    incoming_restart: Option<restart::Incoming>,
    update_busy: bool,
    update_available: Option<crate::updater::Manifest>,
    update_message: String,
    show_updates: bool,
    updates_button_rect: Option<egui::Rect>,
    smoke_test: bool,
    smoke_stage: u8,
    smoke_started: Instant,
}

impl PoetApp {
    pub fn new(cc: &eframe::CreationContext<'_>, incoming: Option<restart::Incoming>) -> Self {
        configure_style(&cc.egui_ctx);
        let smoke_test = std::env::args().any(|arg| arg == "--smoke-test");
        let handoff = incoming.as_ref().map(|incoming| incoming.payload.clone());
        let recovery = if let Some(payload) = &handoff {
            Some(payload.session.clone())
        } else if smoke_test {
            None
        } else {
            storage::load_recovery()
        };
        let mut recovered = recovery.is_some();
        let default_folder = storage::default_dictionary_folder();
        let mut session = recovery.unwrap_or_else(|| storage::Session {
            text: SAMPLE.to_owned(),
            document_title: None,
            dictionary_folder: default_folder.to_string_lossy().into_owned(),
            rhyme_links: Vec::new(),
            last_document_folder: None,
            file_path: None,
            compact_rhymes: false,
            stress_marks: Vec::new(),
            stress_mode: false,
            meter_mode: false,
            meter_overrides: Vec::new(),
        });
        let history_temp =
            smoke_test.then(|| tempfile::tempdir().expect("smoke-test history directory"));
        let history_path = history_temp
            .as_ref()
            .map(|dir| dir.path().to_owned())
            .unwrap_or_else(|| storage::data_directory().join("history"));
        let repaint = cc.egui_ctx.clone();
        let fallback_path = session.file_path.clone();
        let (history, document_id, file_path, history_error) =
            match history::History::open(history_path, move || repaint.request_repaint()) {
                Ok((history, previous, warning)) => {
                    let (document, path) = if let Some(payload) = &handoff {
                        (
                            payload.document_id.clone(),
                            payload.session.file_path.clone(),
                        )
                    } else if let Some((summary, snapshot)) = previous {
                        session.text = snapshot.text;
                        session.document_title = snapshot.title;
                        session.rhyme_links = snapshot.rhyme_links;
                        session.stress_marks = snapshot.stress_marks;
                        session.meter_overrides = snapshot.meter_overrides;
                        recovered = true;
                        (summary.document, summary.path)
                    } else {
                        (history::new_document(), fallback_path.clone())
                    };
                    (Some(history), document, path, warning)
                }
                Err(error) => (None, history::new_document(), fallback_path, Some(error)),
            };
        let saved_folder = PathBuf::from(&session.dictionary_folder);
        let legacy_default = storage::app_directory().join("Rhymes 3");
        if session.dictionary_folder.trim().is_empty()
            || (saved_folder == legacy_default
                && !saved_folder.join("rus_rhymes.dic").is_file()
                && default_folder.join("rus_rhymes.dic").is_file())
        {
            session.dictionary_folder = default_folder.to_string_lossy().into_owned();
        }
        let (tx, rx) = mpsc::channel();
        let mut app = Self {
            history,
            _history_temp: history_temp,
            document_id,
            history_error,
            show_history: false,
            history_all: false,
            history_limit: 200,
            history_entries: Vec::new(),
            history_selected: None,
            history_preview: None,
            saved_text: Some(session.text.clone()),
            saved_title: session.document_title.clone(),
            document_title: session.document_title,
            meter_mode: session.meter_mode,
            meter_scope: meter::Scope::Section,
            meter_line: None,
            meter_overrides: session.meter_overrides,
            meter_analysis: None,
            stress_mode: session.stress_mode,
            stress_marks: session.stress_marks,
            stress_text: session.text.clone(),
            stress_analysis: None,
            stress_ticket: 0,
            stress_due: None,
            stress_loading: false,
            stress_selected: None,
            stress_popup_pos: egui::pos2(350.0, 200.0),
            poem_editor_id: None,
            stress_focus: false,
            text: session.text,
            file_path,
            last_document_folder: session.last_document_folder,
            revision: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            last_typing: None,
            selection: None,
            anchor: String::new(),
            anchor_line: None,
            rhyme_links: session.rhyme_links,
            match_syllables: true,
            closer_only: false,
            compact_rhymes: session.compact_rhymes,
            reveal_candidate: false,
            syllable_count: 3,
            part_of_speech: String::new(),
            candidates: Vec::new(),
            chosen: None,
            lexicon: None,
            parts_of_speech: Vec::new(),
            dictionary_folder: session.dictionary_folder,
            dictionary_error: None,
            dictionary_loading: false,
            dictionary_ticket: 0,
            show_dictionary: false,
            search_ticket: 0,
            search_loading: false,
            search_due: None,
            tx,
            rx,
            status: if recovered {
                "Восстановлен последний текст.".into()
            } else {
                "Нажмите на последнее слово строки, чтобы подобрать замену.".into()
            },
            status_is_error: false,
            autosave_due: None,
            pending_action: None,
            allow_close: false,
            requested_cursor: None,
            requested_scroll: None,
            editor_scroll: 0.0,
            visible_section: None,
            restart_available: false,
            restart_executable: std::env::current_exe().map_err(|e| e.to_string()),
            restart_state: None,
            restart_committed: false,
            incoming_restart: incoming,
            update_busy: false,
            update_available: None,
            update_message: String::new(),
            show_updates: false,
            updates_button_rect: None,
            smoke_test,
            smoke_stage: 0,
            smoke_started: Instant::now(),
        };
        if let Some(payload) = handoff {
            app.document_id = payload.document_id;
            app.saved_text = payload.saved_text;
            app.saved_title = payload.saved_title;
            app.undo = payload.undo;
            app.redo = payload.redo;
            app.requested_cursor = payload.cursor;
        } else if recovered {
            app.restore_recovered_document(app.snapshot(), app.file_path.clone());
        }
        app.rhyme_links.retain(|link| link.is_valid(&app.text));
        stress::retain_valid(&app.text, &mut app.stress_marks);
        meter::retain_valid(&app.text, &mut app.meter_overrides);
        if app.incoming_restart.is_some() {
            return app;
        }
        app.record_revision("Начало сессии");
        if std::env::args().any(|arg| arg == "--smoke-restart") {
            return app;
        }
        app.schedule_stress();
        if !recovered {
            if let Some(at) = app.text.find("фуража") {
                app.select_word(at);
            }
        }
        app.load_dictionary(&cc.egui_ctx);
        if !smoke_test {
            app.check_updates(&cc.egui_ctx);
        }
        app
    }

    fn smoke_restart(&mut self, ctx: &egui::Context) {
        if self.smoke_stage == 2 {
            return;
        }
        let directory = storage::app_directory();
        let result = (|| -> Result<(), String> {
            if std::env::args().any(|arg| arg == "--smoke-restart-child") {
                let expected: restart::Payload = serde_json::from_slice(
                    &std::fs::read(directory.join("expected.json")).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                if serde_json::to_value(self.recovery_session()).unwrap()
                    != serde_json::to_value(&expected.session).unwrap()
                    || self.saved_text != expected.saved_text
                    || self.saved_title != expected.saved_title
                    || self.document_id != expected.document_id
                    || self.requested_cursor != expected.cursor
                    || serde_json::to_value(&self.undo).unwrap()
                        != serde_json::to_value(&expected.undo).unwrap()
                    || serde_json::to_value(&self.redo).unwrap()
                        != serde_json::to_value(&expected.redo).unwrap()
                    || !self.is_dirty()
                {
                    let actual = restart::Payload {
                        session: self.recovery_session(),
                        document_id: self.document_id.clone(),
                        saved_text: self.saved_text.clone(),
                        saved_title: self.saved_title.clone(),
                        undo: self.undo.clone(),
                        redo: self.redo.clone(),
                        cursor: self.requested_cursor,
                    };
                    let _ = std::fs::write(
                        directory.join("actual.json"),
                        serde_json::to_vec(&actual).unwrap(),
                    );
                    return Err(
                        "session/baseline/undo/cursor transfer mismatch (see actual.json)".into(),
                    );
                }
                if storage::read_text(self.file_path.as_ref().unwrap())? != "Оригинал\n" {
                    return Err("restart saved the document without consent".into());
                }
                // Ensure the child can still save to the original path, only on explicit Save.
                if !self.save(false) || self.is_dirty() {
                    return Err("explicit save after restart failed".into());
                }
                if storage::read_text(self.file_path.as_ref().unwrap())? != self.text {
                    return Err("explicit save wrote the wrong file".into());
                }
                Ok(())
            } else {
                if self.smoke_stage != 0 {
                    return Err(self.update_message.clone());
                }
                let original = directory.join("original.txt");
                storage::write_text(&original, "Оригинал\n")?;
                self.replace_document("Оригинал\n".into(), Some(original));
                self.text = "Не спеша\nФуража\n".into();
                self.document_title = Some("Несохранённая песня".into());
                self.after_document_change("Правка до перезапуска");
                self.rhyme_links
                    .push(core::RhymeLink::new(&self.text, 1, Some(0), "спеша".into()).unwrap());
                self.stress_marks.push(stress::Mark {
                    start: self.text.find("Фуража").unwrap(),
                    word: "Фуража".into(),
                    vowel: Some(2),
                });
                self.meter_overrides.push(meter::Override {
                    scope: meter::Scope::Line,
                    start: 0,
                    guard: Some("Не спеша".into()),
                    choice: meter::Choice::Regular(meter::Meter::Iamb),
                    feet: Some(2),
                });
                self.stress_mode = true;
                self.meter_mode = true;
                self.compact_rhymes = true;
                self.undo = vec![self.snapshot()];
                self.redo = vec![self.snapshot()];
                self.record_revision("Разметка до перезапуска");
                self.request_action(PendingAction::Restart, ctx);
                if !matches!(self.pending_action, Some(PendingAction::Restart))
                    || self.restart_state.is_some()
                {
                    return Err("dirty restart did not ask to save".into());
                }
                self.pending_action = None; // Cancel leaves the editor intact.
                if !self.is_dirty() {
                    return Err("cancel cleared dirty state".into());
                }
                // Simulate self-replace, which relocates the running EXE on Windows.
                let executable = self.restart_executable.clone()?;
                let replacement = directory.join("replacement.exe");
                std::fs::copy(&executable, &replacement).map_err(|e| e.to_string())?;
                self_replace::self_replace(&replacement).map_err(|e| e.to_string())?;
                let cursor_id = egui::Id::new("restart-test-cursor");
                let mut cursor_state =
                    egui::TextEdit::load_state(ctx, cursor_id).unwrap_or_default();
                cursor_state
                    .cursor
                    .set_char_range(Some(egui::text::CCursorRange::one(
                        egui::text::CCursor::new(5),
                    )));
                cursor_state.store(ctx, cursor_id);
                self.poem_editor_id = Some(cursor_id);
                let expected = restart::Payload {
                    session: self.recovery_session(),
                    document_id: self.document_id.clone(),
                    saved_text: self.saved_text.clone(),
                    saved_title: self.saved_title.clone(),
                    undo: self.undo.clone(),
                    redo: self.redo.clone(),
                    cursor: Some(5),
                };
                std::fs::write(
                    directory.join("expected.json"),
                    serde_json::to_vec(&expected).unwrap(),
                )
                .map_err(|e| e.to_string())?;
                self.request_action(PendingAction::Restart, ctx);
                let action = self
                    .pending_action
                    .take()
                    .ok_or("second save prompt missing")?;
                self.execute_action(action, ctx); // Continue without saving.
                self.smoke_stage = 1;
                return Ok(());
            }
        })();
        if std::env::args().any(|arg| arg == "--smoke-restart-child") || result.is_err() {
            let report = match result { Ok(()) => "PASS: restart after self-replace, unsaved UTF-8 text/title/annotations/baseline/undo/redo, save prompt/cancel, explicit save to original".into(), Err(e) => format!("FAIL: {e}") };
            let _ = std::fs::write(directory.join("restart-result.txt"), report);
            self.smoke_stage = 2;
            self.allow_close = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(Duration::from_millis(50));
    }

    fn start_restart(&mut self, ctx: &egui::Context) {
        if self.restart_state.is_some() {
            return;
        }
        self.record_revision("Перед перезапуском");
        let preparation = (|| {
            let executable = self.restart_executable.clone()?;
            let checkpoint = self
                .history
                .as_ref()
                .map(|history| history.checkpoint())
                .transpose()?;
            let cursor = self
                .poem_editor_id
                .and_then(|id| egui::TextEdit::load_state(ctx, id))
                .and_then(|state| state.cursor.char_range())
                .map(|range| range.primary.index);
            let payload = restart::Payload {
                session: self.recovery_session(),
                document_id: self.document_id.clone(),
                saved_text: self.saved_text.clone(),
                saved_title: self.saved_title.clone(),
                undo: self.undo.clone(),
                redo: self.redo.clone(),
                cursor,
            };
            let (tx, rx) = mpsc::channel();
            let repaint = ctx.clone();
            let smoke_test = self.smoke_test;
            std::thread::spawn(move || {
                let _ = tx.send(restart::prepare(
                    &executable,
                    payload,
                    checkpoint,
                    smoke_test,
                ));
                repaint.request_repaint();
            });
            Ok::<_, String>(RestartState::Preparing(rx))
        })();
        match preparation {
            Ok(state) => self.restart_state = Some(state),
            Err(error) => self.restart_error(error),
        }
        ctx.request_repaint();
    }

    fn restart_error(&mut self, error: String) {
        self.update_message =
            format!("Перезапуск не выполнен: {error}. Текст остаётся в этом окне.");
        self.set_status(self.update_message.clone(), true);
        self.show_updates = true;
    }

    // Freeze both editors until the new window acknowledges ownership of the session.
    fn restart_frame(&mut self, ctx: &egui::Context) -> bool {
        if self.restart_committed {
            return true;
        }
        if let Some(incoming) = &mut self.incoming_restart {
            match incoming.poll() {
                Ok(true) => {
                    self.incoming_restart = None;
                    if self.smoke_test && std::env::args().any(|arg| arg == "--smoke-restart-child")
                    {
                        return false;
                    }
                    self.record_revision("Перезапуск после обновления");
                    self.schedule_stress();
                    self.load_dictionary(ctx);
                    if !self.smoke_test {
                        self.check_updates(ctx);
                    }
                    self.set_status(
                        "Редактор перезапущен. Текст и история восстановлены.",
                        false,
                    );
                    return false;
                }
                Err(_) => {
                    self.allow_close = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return true;
                }
                Ok(false) => {}
            }
        } else if let Some(state) = self.restart_state.take() {
            let next = match state {
                RestartState::Preparing(rx) => match rx.try_recv() {
                    Ok(Ok(pending)) => Ok(Some(RestartState::Waiting(pending))),
                    Ok(Err(error)) => Err(error),
                    Err(mpsc::TryRecvError::Empty) => Ok(Some(RestartState::Preparing(rx))),
                    Err(_) => Err("Подготовка перезапуска прервана".into()),
                },
                RestartState::Waiting(mut pending) => match pending.poll() {
                    Ok(true) => {
                        self.restart_committed = true;
                        self.allow_close = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        Ok(None)
                    }
                    Ok(false) => Ok(Some(RestartState::Waiting(pending))),
                    Err(error) => Err(error),
                },
            };
            match next {
                Ok(state) => self.restart_state = state,
                Err(error) => {
                    self.restart_error(error);
                    return false;
                }
            }
        } else {
            return false;
        }
        if !self.allow_close && ctx.input(|input| input.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Перезапускаем Poetrhymer. Сохраняем историю и переносим текст…");
            });
        });
        ctx.request_repaint_after(Duration::from_millis(50));
        true
    }

    fn recovery_session(&self) -> storage::Session {
        storage::Session {
            text: self.text.clone(),
            document_title: self.document_title.clone(),
            dictionary_folder: self.dictionary_folder.clone(),
            rhyme_links: self.rhyme_links.clone(),
            last_document_folder: self.last_document_folder.clone(),
            file_path: self.file_path.clone(),
            compact_rhymes: self.compact_rhymes,
            stress_marks: self.stress_marks.clone(),
            stress_mode: self.stress_mode,
            meter_mode: self.meter_mode,
            meter_overrides: self.meter_overrides.clone(),
        }
    }

    fn record_revision(&mut self, reason: &str) {
        let snapshot = self.snapshot();
        if let Some(history) = &mut self.history {
            history.record(&self.document_id, snapshot, self.file_path.clone(), reason);
        }
    }

    fn receive_history(&mut self) {
        let events: Vec<_> = self
            .history
            .as_ref()
            .map(|history| history.rx.try_iter().collect())
            .unwrap_or_default();
        for event in events {
            match event {
                history::Event::Saved => {
                    if let Some(history) = &mut self.history {
                        history.pending = history.pending.saturating_sub(1);
                    }
                    if self
                        .history_error
                        .as_deref()
                        .is_some_and(|e| e.starts_with("Не удалось записать историю"))
                    {
                        self.history_error = None;
                    }
                }
                history::Event::Error(error) => self.history_error = Some(error),
                history::Event::Catalogue(entries) => self.history_entries = entries,
                history::Event::Preview(id, snapshot) => {
                    if self.history_selected.as_deref() == Some(&id) {
                        self.history_preview = Some(snapshot);
                    }
                }
            }
        }
    }

    fn history_window(&mut self, ctx: &egui::Context) {
        if !self.show_history {
            return;
        }
        let mut open = true;
        let mut restore = None;
        egui::Window::new("История правок")
            .open(&mut open)
            .default_size([830.0, 580.0])
            .default_pos(egui::pos2(220.0, 90.0))
            .min_size([600.0, 360.0])
            .max_size(ctx.screen_rect().size() - egui::vec2(40.0, 40.0))
            .collapsible(false)
            .show(ctx, |ui| {
                ui.set_min_width((ctx.screen_rect().width() - 80.0).clamp(520.0, 800.0));
                ui.label(
                    "Версии хранятся на этом компьютере. Восстановление добавляет новую правку.",
                );
                if let Some(error) = &self.history_error {
                    ui.colored_label(Color32::LIGHT_RED, error);
                }
                ui.horizontal(|ui| {
                    if ui
                        .checkbox(&mut self.history_all, "Все документы и сеансы")
                        .changed()
                    {
                        self.history_limit = 200;
                        self.history_selected = None;
                        self.history_preview = None;
                    }
                    if ui.button("Обновить список").clicked() {
                        if let Some(history) = &self.history {
                            history.catalogue();
                        }
                    }
                    if self.history.as_ref().is_some_and(|h| h.pending > 0) {
                        ui.spinner();
                        ui.label("Записываем правки…");
                    }
                });
                ui.separator();
                let selected_summary = self
                    .history_entries
                    .iter()
                    .find(|entry| Some(&entry.id) == self.history_selected.as_ref())
                    .cloned();
                let height = (ctx.screen_rect().height() - 290.0).clamp(160.0, 420.0);
                ui.horizontal_top(|ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(280.0, height),
                        egui::Layout::top_down(egui::Align::LEFT),
                        |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("revision_list")
                                .max_height(height)
                                .show(ui, |ui| {
                                    let entries: Vec<_> = self
                                        .history_entries
                                        .iter()
                                        .filter(|entry| {
                                            self.history_all || entry.document == self.document_id
                                        })
                                        .collect();
                                    if entries.is_empty() {
                                        ui.label("Список пока пуст. Обновите после записи правок.");
                                    }
                                    for entry in entries.iter().take(self.history_limit) {
                                        let label = format!(
                                            "{} · {}\n{} · {} знаков",
                                            revision_date(entry.time),
                                            entry.title,
                                            entry.reason,
                                            entry.chars
                                        );
                                        if ui
                                            .selectable_label(
                                                self.history_selected.as_ref() == Some(&entry.id),
                                                label,
                                            )
                                            .clicked()
                                        {
                                            self.history_selected = Some(entry.id.clone());
                                            self.history_preview = None;
                                            if let Some(history) = &self.history {
                                                history.preview(&entry.id);
                                            }
                                        }
                                    }
                                    if entries.len() > self.history_limit
                                        && ui.button("Показать ещё 200").clicked()
                                    {
                                        self.history_limit += 200;
                                    }
                                });
                        },
                    );
                    ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width().max(200.0), height),
                        egui::Layout::top_down(egui::Align::LEFT),
                        |ui| {
                            if let (Some(summary), Some(snapshot)) =
                                (&selected_summary, &self.history_preview)
                            {
                                ui.label(RichText::new(&summary.title).strong());
                                if !snapshot.stress_marks.is_empty() {
                                    ui.label(
                                        RichText::new(format!(
                                            "Ручных отметок ударения: {}",
                                            snapshot.stress_marks.len()
                                        ))
                                        .small()
                                        .color(MUTED),
                                    );
                                }
                                if let Some(path) = &summary.path {
                                    ui.label(
                                        RichText::new(path.display().to_string())
                                            .small()
                                            .color(MUTED),
                                    );
                                }
                                egui::ScrollArea::both()
                                    .id_salt("revision_preview")
                                    .max_height(height - 70.0)
                                    .show(ui, |ui| {
                                        let mut text = snapshot.text.as_str();
                                        ui.add(
                                            egui::TextEdit::multiline(&mut text)
                                                .desired_width(ui.available_width())
                                                .font(egui::TextStyle::Monospace),
                                        );
                                    });
                            } else {
                                ui.label("Выберите версию для просмотра.");
                            }
                        },
                    );
                });
                ui.separator();
                let current = selected_summary
                    .as_ref()
                    .is_some_and(|s| s.document == self.document_id);
                if ui
                    .add_enabled(
                        self.history_preview.is_some(),
                        egui::Button::new(if current {
                            "Восстановить эту версию"
                        } else {
                            "Открыть версию как новый текст"
                        }),
                    )
                    .clicked()
                {
                    restore = self
                        .history_preview
                        .clone()
                        .map(|snapshot| (current, snapshot));
                }
            });
        self.show_history = open;
        if let Some((current, snapshot)) = restore {
            if current {
                self.restore_version(snapshot);
            } else {
                self.request_action(PendingAction::CopyRevision(snapshot), ctx);
            }
            self.show_history = false;
        }
    }

    fn restore_version(&mut self, snapshot: DocumentState) {
        self.record_revision("Перед восстановлением");
        self.push_undo(self.snapshot());
        self.restore_snapshot(snapshot);
        self.last_typing = None;
        self.after_document_change("Восстановление версии");
        self.requested_cursor = Some(0);
        self.set_status(
            "Версия восстановлена. Ctrl+Z — отменить. Файл изменится после сохранения.",
            false,
        );
    }

    fn check_updates(&mut self, ctx: &egui::Context) {
        if self.update_busy {
            return;
        }
        self.update_busy = true;
        self.update_message = "Проверяем обновления…".into();
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Background::UpdateCheck(crate::updater::check()));
            ctx.request_repaint();
        });
    }

    fn updates_window(&mut self, ctx: &egui::Context) {
        if !self.show_updates {
            return;
        }
        let mut open = true;
        let position = self
            .updates_button_rect
            .map(|rect| rect.right_bottom() + egui::vec2(0.0, 8.0))
            .unwrap_or_else(|| ctx.screen_rect().right_top() + egui::vec2(-20.0, 55.0));
        egui::Window::new("Обновления Poetrhymer")
            .open(&mut open)
            .pivot(egui::Align2::RIGHT_TOP)
            .fixed_pos(position)
            .constrain_to(ctx.screen_rect())
            .default_width(300.0)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
            ui.label(format!("Текущая версия: {}", env!("CARGO_PKG_VERSION")));
            ui.label(&self.update_message);
            if self.restart_available && ui.button("Перезапустить Poetrhymer").clicked() {
                self.request_action(PendingAction::Restart, ctx);
            }
            if self.update_busy { ui.spinner(); }
            if let Some(manifest) = self.update_available.clone() {
                if ui.add_enabled(!self.update_busy, egui::Button::new("Скачать и установить")).clicked() {
                    self.update_busy = true;
                    self.update_message = "Загружаем и проверяем подпись…".into();
                    let tx = self.tx.clone(); let ctx = ctx.clone();
                    std::thread::spawn(move || { let _ = tx.send(Background::UpdateInstalled(crate::updater::install(&manifest))); ctx.request_repaint(); });
                }
                ui.label("Перед установкой проверяются подпись и контрольная сумма. После установки можно перезапустить редактор. Несохранённый текст и история будут восстановлены.");
            }
            if ui.add_enabled(!self.update_busy && !self.restart_available, egui::Button::new("Проверить обновления")).clicked() { self.check_updates(ctx); }
            ui.hyperlink_to("Страница выпусков", format!("{}/releases", crate::updater::REPOSITORY));
        });
        self.show_updates = open;
    }

    fn set_status(&mut self, message: impl Into<String>, error: bool) {
        self.status = message.into();
        self.status_is_error = error;
    }

    fn is_dirty(&self) -> bool {
        self.saved_text.as_deref() != Some(self.text.as_str())
            || self.document_title != self.saved_title
    }

    fn load_dictionary(&mut self, ctx: &egui::Context) {
        self.dictionary_ticket += 1;
        self.dictionary_loading = true;
        self.dictionary_error = None;
        self.lexicon = None;
        self.schedule_stress();
        self.parts_of_speech.clear();
        self.invalidate_search();
        let folder = PathBuf::from(self.dictionary_folder.trim());
        let ticket = self.dictionary_ticket;
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = Lexicon::load_rhymes(&folder).map(|lexicon| {
                let parts = lexicon
                    .entries
                    .iter()
                    .map(|entry| entry.part_of_speech.as_str())
                    .filter(|part| !part.is_empty())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .map(str::to_owned)
                    .collect();
                (Arc::new(lexicon), parts)
            });
            let _ = tx.send(Background::Dictionary(ticket, result));
            ctx.request_repaint();
        });
        self.autosave_due = Some(Instant::now() + Duration::from_secs(1));
    }

    fn invalidate_search(&mut self) {
        self.search_ticket += 1;
        self.search_loading = false;
        self.search_due = None;
        self.candidates.clear();
        self.chosen = None;
    }

    fn schedule_search(&mut self) {
        self.invalidate_search();
        if self.selection.is_some() && !self.anchor.trim().is_empty() {
            self.search_due = Some(Instant::now() + Duration::from_millis(50));
        }
    }

    fn start_search(&mut self, ctx: &egui::Context) {
        self.search_due = None;
        let Some(lexicon) = self.lexicon.clone() else {
            return;
        };
        if self.selection.is_none() || self.anchor.trim().is_empty() {
            return;
        }
        let ticket = self.search_ticket;
        let anchor = self.anchor.trim();
        let anchor = saved_anchor_stress(&self.text, self.anchor_line, anchor, &self.stress_marks)
            .map_or_else(|| anchor.to_owned(), |vowel| core::with_stress(anchor, vowel));
        let filter = core::SearchFilter {
            syllables: self.match_syllables.then_some(self.syllable_count),
            part_of_speech: (!self.part_of_speech.is_empty()).then(|| self.part_of_speech.clone()),
        };
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        self.search_loading = true;
        std::thread::spawn(move || {
            let candidates = core::search_rhymes(&lexicon, &anchor, &filter, 150);
            let _ = tx.send(Background::Search(ticket, candidates));
            ctx.request_repaint();
        });
    }

    fn receive_background(&mut self) {
        while let Ok(message) = self.rx.try_recv() {
            match message {
                Background::Stress(ticket, analysis, metrical) if ticket == self.stress_ticket => {
                    self.meter_analysis = Some(metrical);
                    self.stress_loading = false;
                    self.stress_analysis = Some(analysis);
                }
                Background::Dictionary(ticket, result) if ticket == self.dictionary_ticket => {
                    self.dictionary_loading = false;
                    match result {
                        Ok((lexicon, parts)) => {
                            self.lexicon = Some(lexicon);
                            self.parts_of_speech = parts;
                            self.schedule_search();
                            self.schedule_stress();
                        }
                        Err(error) => {
                            self.dictionary_error = Some(error);
                            self.show_dictionary = true;
                        }
                    }
                }
                Background::Search(ticket, mut candidates) if ticket == self.search_ticket => {
                    self.search_loading = false;
                    if let Some(selection) = &self.selection {
                        candidates.retain(|candidate| {
                            candidate.word.to_lowercase() != selection.word.to_lowercase()
                        });
                    }
                    self.candidates = candidates;
                    self.chosen = None;
                }
                Background::UpdateCheck(result) => {
                    self.update_busy = false;
                    match result {
                        Ok(Some(manifest)) => {
                            self.update_message = format!("Доступна версия {}", manifest.version);
                            self.update_available = Some(manifest);
                            self.show_updates = true;
                        }
                        Ok(None) => self.update_message = "Установлена актуальная версия".into(),
                        Err(error) => self.update_message = format!("Проверка недоступна: {error}"),
                    }
                }
                Background::UpdateInstalled(result) => {
                    self.update_busy = false;
                    self.update_message = match result {
                        Ok(()) => {
                            self.update_available = None;
                            self.restart_available = true;
                            "Обновление установлено.".into()
                        }
                        Err(error) => format!("Ошибка обновления: {error}"),
                    };
                }
                _ => {}
            }
        }
    }

    fn select_word(&mut self, at: usize) {
        let lines = core::parse_lines(&self.text);
        let Some(line) = lines.iter().find(|line| {
            line.last_word
                .as_ref()
                .is_some_and(|span| at >= span.start && at <= span.end)
        }) else {
            return;
        };
        let span = line.last_word.clone().unwrap();
        let word = self.text[span.clone()].to_owned();
        self.syllable_count = core::syllables(&word).max(1);
        self.selection = Some(Selection {
            span,
            word,
            line: line.index,
            revision: self.revision,
        });
        if let Some(link) = self
            .rhyme_links
            .iter()
            .find(|link| link.target == line.index && link.is_valid(&self.text))
        {
            self.anchor = link.word.clone();
            self.anchor_line = link.anchor;
        } else if let Some(index) = core::suggest_anchor(&self.text, line.index) {
            self.anchor = self.text[lines[index].last_word.clone().unwrap()].to_owned();
            self.anchor_line = Some(index);
        } else {
            self.anchor.clear();
            self.anchor_line = None;
        }
        self.schedule_search();
    }

    fn remember_anchor(&mut self) {
        let Some(selection) = &self.selection else {
            return;
        };
        let target = selection.line;
        self.rhyme_links.retain(|link| link.target != target);
        if let Some(link) =
            core::RhymeLink::new(&self.text, target, self.anchor_line, self.anchor.clone())
        {
            self.rhyme_links.push(link);
        }
        self.autosave_due = Some(Instant::now() + Duration::from_millis(800));
        self.record_revision("Опора рифмы");
    }

    fn after_document_change(&mut self, reason: &str) {
        stress::rebase(&self.stress_text, &self.text, &mut self.stress_marks);
        meter::rebase(&self.stress_text, &self.text, &mut self.meter_overrides);
        self.stress_text = self.text.clone();
        self.stress_selected = None;
        self.schedule_stress();
        self.rhyme_links.retain(|link| link.is_valid(&self.text));
        self.revision += 1;
        self.selection = None;
        self.anchor_line = None;
        self.invalidate_search();
        self.autosave_due = Some(Instant::now() + Duration::from_millis(800));
        self.record_revision(reason);
    }

    fn snapshot(&self) -> DocumentState {
        DocumentState {
            text: self.text.clone(),
            title: self.document_title.clone(),
            rhyme_links: self.rhyme_links.clone(),
            stress_marks: self.stress_marks.clone(),
            meter_overrides: self.meter_overrides.clone(),
        }
    }

    fn restore_snapshot(&mut self, state: DocumentState) {
        self.text = state.text;
        self.document_title = state.title;
        self.rhyme_links = state.rhyme_links;
        self.stress_marks = state.stress_marks;
        self.meter_overrides = state.meter_overrides;
        meter::retain_valid(&self.text, &mut self.meter_overrides);
        self.stress_text = self.text.clone();
        stress::retain_valid(&self.text, &mut self.stress_marks);
        self.stress_selected = None;
        self.schedule_stress();
    }

    fn restore_recovered_document(&mut self, state: DocumentState, path: Option<PathBuf>) {
        self.restore_snapshot(state);
        self.file_path = path;
        self.saved_text = storage::saved_text_after_recovery(&self.text, self.file_path.as_deref());
        self.saved_title = self.document_title.clone();
    }

    fn format_document(&mut self) {
        let formatted = formatter::format_paste(&self.text, true);
        let title = formatted.title.or_else(|| self.document_title.clone());
        if formatted.text == self.text && title == self.document_title {
            self.set_status("Текст уже в обычном формате", false);
            return;
        }
        let previous = self.snapshot();
        self.text = formatted.text;
        self.document_title = title;
        self.push_undo(previous);
        self.last_typing = None;
        self.after_document_change("Форматирование");
        self.requested_cursor = Some(0);
        self.set_status(
            "Markdown преобразован в обычный текст. Ctrl+Z — отменить.",
            false,
        );
    }

    fn push_undo(&mut self, previous: DocumentState) {
        self.undo.push(previous);
        if self.undo.len() > 150 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn undo(&mut self) {
        if let Some(previous) = self.undo.pop() {
            let current = self.snapshot();
            self.redo.push(current);
            self.restore_snapshot(previous);
            self.last_typing = None;
            self.after_document_change("Отмена");
            self.set_status("Изменение отменено", false);
        }
    }

    fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            let current = self.snapshot();
            self.undo.push(current);
            self.restore_snapshot(next);
            self.last_typing = None;
            self.after_document_change("Повтор");
            self.set_status("Изменение повторено", false);
        }
    }

    fn apply_candidate(&mut self) {
        let Some(selection) = &self.selection else {
            return;
        };
        let Some(candidate) = self.chosen.and_then(|index| self.candidates.get(index)) else {
            return;
        };
        if selection.revision != self.revision
            || self.text.get(selection.span.clone()) != Some(selection.word.as_str())
        {
            self.set_status("Строка изменилась. Выберите последнее слово ещё раз.", true);
            self.selection = None;
            return;
        }
        let target_start = selection.span.start;
        let was_manual = self
            .rhyme_links
            .iter()
            .any(|link| link.target == selection.line);
        let old_word = selection.word.clone();
        let new_word = candidate.word.clone();
        let anchor = self.anchor.clone();
        let anchor_line = self.anchor_line;
        match core::replace_word(&self.text, selection.span.clone(), &new_word) {
            Ok(text) => {
                let previous = self.snapshot();
                self.text = text;
                self.push_undo(previous);
                self.last_typing = None;
                self.after_document_change("Замена рифмы");
                self.requested_cursor = Some(core::byte_to_char(&self.text, target_start));
                self.select_word(target_start);
                self.anchor = anchor;
                self.anchor_line = anchor_line;
                if was_manual {
                    self.remember_anchor();
                }
                self.schedule_search();
                self.set_status(
                    format!("«{old_word}» → «{new_word}». Ctrl+Z — отменить."),
                    false,
                );
            }
            Err(error) => self.set_status(error, true),
        }
    }

    fn save(&mut self, save_as: bool) -> bool {
        let default_filename = formatter::suggested_filename(self.document_title.as_deref());
        let path = if save_as || self.file_path.is_none() {
            rfd::FileDialog::new()
                .set_title("Сохранить текст")
                .set_directory(
                    self.file_path
                        .as_deref()
                        .and_then(std::path::Path::parent)
                        .map(PathBuf::from)
                        .or_else(|| self.last_document_folder.clone())
                        .filter(|path| path.is_dir())
                        .unwrap_or_else(storage::documents_directory),
                )
                .add_filter("Текст UTF-8", &["txt"])
                .set_file_name(
                    self.file_path
                        .as_ref()
                        .and_then(|path| path.file_name())
                        .and_then(|name| name.to_str())
                        .unwrap_or(default_filename.as_str()),
                )
                .save_file()
        } else {
            self.file_path.clone()
        };
        let Some(path) = path else { return false };
        self.save_to(path)
    }

    fn save_to(&mut self, path: PathBuf) -> bool {
        match storage::write_text(&path, &self.text) {
            Ok(()) => {
                self.last_document_folder = path.parent().map(std::path::Path::to_owned);
                self.file_path = Some(path);
                self.saved_text = Some(self.text.clone());
                self.saved_title = self.document_title.clone();
                self.record_revision("Сохранение файла");
                self.autosave_due = Some(Instant::now() + Duration::from_millis(800));
                self.set_status("Текст сохранён", false);
                true
            }
            Err(error) => {
                self.set_status(error, true);
                false
            }
        }
    }

    fn request_action(&mut self, action: PendingAction, ctx: &egui::Context) {
        if self.is_dirty() {
            self.pending_action = Some(action);
        } else {
            self.execute_action(action, ctx);
        }
    }

    fn execute_action(&mut self, action: PendingAction, ctx: &egui::Context) {
        match action {
            PendingAction::Restart => self.start_restart(ctx),
            PendingAction::Close => {
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            PendingAction::New => self.replace_document(String::new(), None),
            PendingAction::CopyRevision(snapshot) => {
                self.replace_document(snapshot.text.clone(), None);
                self.restore_snapshot(snapshot);
                self.saved_text = None;
                self.saved_title = None;
                self.after_document_change("Копия версии");
                self.set_status(
                    "Версия открыта как новый текст. Исходная история сохранена.",
                    false,
                );
            }
            PendingAction::Open(path) => match storage::read_text(&path) {
                Ok(text) => {
                    let metadata = self
                        .history
                        .as_ref()
                        .map(|history| history.file_metadata(path.clone(), text.clone()))
                        .transpose();
                    self.replace_document(text, Some(path));
                    match metadata {
                        Ok(Some(Some(state))) => {
                            self.restore_snapshot(state);
                            self.saved_title = self.document_title.clone();
                            self.record_revision("Восстановление разметки файла");
                        }
                        Err(error) => self.set_status(
                            format!("Файл открыт, но разметка из истории недоступна: {error}"),
                            true,
                        ),
                        _ => {}
                    }
                }
                Err(error) => self.set_status(error, true),
            },
        }
    }

    fn replace_document(&mut self, text: String, path: Option<PathBuf>) {
        self.stress_marks.clear();
        self.meter_overrides.clear();
        self.meter_line = None;
        self.stress_text = text.clone();
        self.document_id = history::new_document();
        self.rhyme_links.clear();
        self.document_title = None;
        self.saved_title = None;
        self.text = text;
        self.saved_text = Some(self.text.clone());
        if let Some(parent) = path.as_deref().and_then(std::path::Path::parent) {
            self.last_document_folder = Some(parent.to_owned());
        }
        self.file_path = path;
        self.undo.clear();
        self.redo.clear();
        self.last_typing = None;
        self.after_document_change("Открытие / новый документ");
        self.requested_cursor = Some(0);
        self.set_status("Выберите слово в конце строки для подбора рифмы.", false);
    }

    fn open(&mut self, ctx: &egui::Context) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Текст UTF-8", &["txt", "md"])
            .pick_file()
        {
            self.request_action(PendingAction::Open(path), ctx);
        }
    }

    fn toolbar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("toolbar")
            .exact_height(65.0)
            .show(ctx, |ui| {
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new("poetrhymer")
                            .size(24.0)
                            .strong()
                            .color(ACCENT),
                    );
                    ui.add_space(18.0);
                    let name = self
                        .document_title
                        .clone()
                        .or_else(|| {
                            self.file_path
                                .as_ref()
                                .and_then(|path| path.file_name())
                                .map(|name| name.to_string_lossy().into_owned())
                        })
                        .unwrap_or_else(|| "Новый текст".into());
                    let title_width = (ui.available_width() - 640.0).clamp(50.0, 420.0);
                    ui.allocate_ui_with_layout(
                        egui::vec2(title_width, 30.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!(
                                        "{name}{}",
                                        if self.is_dirty() { " •" } else { "" }
                                    ))
                                    .color(MUTED),
                                )
                                .truncate(),
                            )
                            .on_hover_text(&name);
                        },
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(12.0);
                        let updates_button = ui.button("Обновления");
                        self.updates_button_rect = Some(updates_button.rect);
                        if updates_button.clicked() {
                            self.show_updates = !self.show_updates;
                        }
                        if ui.button("Словарь").clicked() {
                            self.show_dictionary = !self.show_dictionary;
                        }
                        if ui.button("История").clicked() {
                            self.show_history = !self.show_history;
                            if let Some(history) = &self.history {
                                history.catalogue();
                            }
                        }
                        if ui.button("Сохранить").on_hover_text("Ctrl+S").clicked() {
                            self.save(false);
                        }
                        if ui.button("Открыть").on_hover_text("Ctrl+O").clicked() {
                            self.open(ctx);
                        }
                        if ui.button("Новый").clicked() {
                            self.request_action(PendingAction::New, ctx);
                        }
                    });
                });
            });
    }

    fn navigate_to(&mut self, byte: usize) {
        self.requested_cursor = Some(core::byte_to_char(&self.text, byte));
        self.requested_scroll = self.requested_cursor;
    }

    fn outline(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("outline")
            .default_width(175.0)
            .width_range(150.0..=250.0)
            .show(ctx, |ui| {
                ui.add_space(20.0);
                ui.label(RichText::new("СТРУКТУРА").size(11.0).strong().color(MUTED));
                ui.add_space(14.0);
                let lines = core::parse_lines(&self.text);
                let sections: Vec<_> = lines
                    .iter()
                    .filter_map(|line| {
                        line.section
                            .as_ref()
                            .map(|title| (title, line.span.start, line.index + 1))
                    })
                    .collect();
                if sections.is_empty() {
                    ui.label(
                        RichText::new(
                            "Заголовки: [Куплет], **Припев** или # Название отдельной строкой.",
                        )
                        .color(MUTED)
                        .size(13.0),
                    );
                }
                egui::ScrollArea::vertical()
                    .id_salt("section_navigation")
                    .max_height(ui.available_height())
                    .show(ui, |ui| {
                        for (title, start, line) in sections {
                            let active = self.visible_section == Some(start);
                            let response = ui
                                .push_id(start, |ui| outline_link(ui, title, active, line))
                                .inner;
                            if response.has_focus() {
                                response.scroll_to_me(None);
                            }
                            if response.clicked() {
                                self.navigate_to(start);
                            }
                        }
                    });
            });
    }

    fn rhyme_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("rhymes").default_width(335.0).width_range(290.0..=460.0).show(ctx, |ui| {
            ui.add_space(18.0);
            ui.label(RichText::new("Рифмы").size(24.0).strong());
            ui.add_space(6.0);
            let Some(selection) = &self.selection else {
                ui.add_space(32.0);
                ui.label(RichText::new("Начните с последнего слова").size(18.0).color(ACCENT));
                ui.add_space(10.0);
                ui.label("Нажмите на подсвеченное слово в конце строки. Здесь появятся варианты замены.");
                return;
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new("ЗАМЕНИТЬ").size(10.0).color(MUTED));
                ui.label(RichText::new(&selection.word).size(18.0).color(ACCENT).strong());
            });
            let line_syllables = core::parse_lines(&self.text).get(selection.line).map_or(0, |line| line.syllables);
            ui.label(RichText::new(core::line_label(&self.text, selection.line)).color(MUTED).size(12.0));
            ui.label(RichText::new(format!("Слоги: в слове {} · в строке {}", core::syllables(&selection.word), line_syllables)).color(MUTED).size(12.0));
            let selected_line = selection.line;
            ui.add_space(15.0);
            let manual = self.rhyme_links.iter().any(|link| link.target == selected_line);
            ui.horizontal(|ui| {
                ui.label(if manual { "Опора выбрана вручную" } else { "Предложенная опора" });
                if manual && ui.small_button("Авто").on_hover_text("Забыть ручной выбор для этой строки").clicked() {
                    self.rhyme_links.retain(|link| link.target != selected_line);
                    self.record_revision("Автоматическая опора");
                    self.autosave_due = Some(Instant::now() + Duration::from_millis(800));
                    let at = self.selection.as_ref().unwrap().span.start;
                    self.select_word(at);
                }
            });
            ui.label("Рифма к слову");
            if ui.add(egui::TextEdit::singleline(&mut self.anchor).desired_width(f32::INFINITY).hint_text("Например, спеша́")).changed() {
                self.anchor_line = None;
                self.remember_anchor();
                self.schedule_search();
            }
            let unaccented: String = self.anchor.chars().filter(|c| *c != '\u{301}').collect();
            let mut stress_choice = None;
            if stress::needs_mark(&unaccented) {
                let selected_stress = core::explicit_stress(&self.anchor).or_else(|| {
                    saved_anchor_stress(&self.text, self.anchor_line, self.anchor.trim(), &self.stress_marks)
                });
                let known = stress::dictionary_choices(&unaccented, self.lexicon.as_deref());
                if !known.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Ударение из словаря:").small().color(MUTED));
                        for index in known {
                            if stress_option(ui, &unaccented, index, selected_stress == Some(index), true, true).clicked() {
                                stress_choice = Some(index);
                            }
                        }
                    });
                }
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Вручную: двойной клик по гласной").small().color(MUTED));
                    let (_, choice) = manual_stress_word(ui, &unaccented, selected_stress);
                    if choice.is_some() { stress_choice = choice; }
                });
            }
            if let Some(stress) = stress_choice {
                self.anchor = core::with_stress(&unaccented, stress);
                self.remember_anchor();
                self.schedule_search();
            }
            let mut anchor_choice = None;
            egui::ComboBox::from_id_salt("anchor_line")
                .width(ui.available_width() - 8.0)
                .selected_text(self.anchor_line.map(|line| format!("Из {}", core::line_label(&self.text, line))).unwrap_or_else(|| "Выбрать строку-опору".into()))
                .show_ui(ui, |ui| {
                    for line in core::parse_lines(&self.text) {
                        if line.index == selected_line { continue }
                        if let Some(span) = line.last_word {
                            let word = self.text[span].to_owned();
                            if ui.selectable_label(self.anchor_line == Some(line.index), format!("{} · {}", core::line_label(&self.text, line.index), word)).clicked() {
                                anchor_choice = Some((line.index, word));
                            }
                        }
                    }
                });
            if let Some((line, word)) = anchor_choice {
                self.anchor_line = Some(line);
                self.anchor = word;
                self.remember_anchor();
                self.schedule_search();
            }
            if let Some(index) = self.anchor_line {
                if let Some(line) = core::parse_lines(&self.text).get(index) {
                    ui.label(RichText::new(self.text[line.span.clone()].trim()).small().color(MUTED));
                }
            }
            ui.add_space(12.0);
            let mut filter_changed = false;
            ui.horizontal(|ui| {
                filter_changed |= ui.checkbox(&mut self.match_syllables, "Слогов в слове").on_hover_text("Число слогов в варианте замены. Изначально берётся из заменяемого слова.").changed();
                filter_changed |= ui.add_enabled(self.match_syllables, egui::DragValue::new(&mut self.syllable_count).range(1..=20)).changed();
            });
            egui::ComboBox::from_id_salt("part_of_speech")
                .width(ui.available_width() - 8.0)
                .selected_text(if self.part_of_speech.is_empty() { "Любая часть речи" } else { &self.part_of_speech })
                .show_ui(ui, |ui| {
                    filter_changed |= ui.selectable_value(&mut self.part_of_speech, String::new(), "Любая часть речи").changed();
                    for part in &self.parts_of_speech {
                        filter_changed |= ui.selectable_value(&mut self.part_of_speech, part.clone(), part).changed();
                    }
                });
            if filter_changed { self.schedule_search(); }
            if ui.checkbox(&mut self.closer_only, "Более созвучные").on_hover_text("Совпадает также согласный перед ударной гласной; оценка звучания в прототипе упрощена.").changed() {
                self.chosen = None;
            }
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);
            let visible_count = self.candidates.iter().filter(|candidate| !self.closer_only || candidate.score >= 120).count();
            if self.dictionary_loading {
                ui.horizontal(|ui| { ui.spinner(); ui.label("Подготавливаем словарь Rhymes…"); });
            } else if self.dictionary_error.is_some() {
                ui.colored_label(Color32::LIGHT_RED, "Словарь не подключён");
                if ui.button("Выбрать папку Rhymes").clicked() { self.show_dictionary = true; }
            } else if self.anchor.trim().is_empty() {
                ui.label(RichText::new("Выберите строку-опору или введите слово.").color(MUTED));
            } else if self.search_loading || self.search_due.is_some() {
                ui.horizontal(|ui| { ui.spinner(); ui.label("Ищем созвучия…"); });
            } else if visible_count == 0 {
                ui.label(RichText::new("Нет подходящих вариантов. Попробуйте снять фильтр слогов или указать ударение в опорном слове.").color(MUTED));
            } else {
                ui.label(RichText::new(format!("Варианты · {}{}", visible_count, if self.candidates.len() == 150 { " из первых 150" } else { "" })).size(12.0).color(MUTED));
            }
            self.rhyme_results(ui, ctx);
        });
    }

    fn rhyme_results(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let old_view = self.compact_rhymes;
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.compact_rhymes, false, "Список");
            ui.selectable_value(&mut self.compact_rhymes, true, "Компактно")
                .on_hover_text("Плотный массив слов с переносом по ширине панели");
        });
        if old_view != self.compact_rhymes {
            self.autosave_due = Some(Instant::now() + Duration::from_millis(800));
            self.reveal_candidate = true;
        }
        let preview = self
            .chosen
            .and_then(|index| self.candidates.get(index))
            .and_then(|candidate| {
                self.selection.as_ref().and_then(|selection| {
                    core::replace_word(&self.text, selection.span.clone(), &candidate.word)
                        .ok()
                        .and_then(|text| {
                            core::parse_lines(&text)
                                .get(selection.line)
                                .map(|line| text[line.span.clone()].trim().to_owned())
                        })
                })
            });
        // Measure and allocate the real footer first; no empty fixed-height reserve.
        if let Some(preview) = preview {
            egui::TopBottomPanel::bottom("rhyme_preview_footer")
                .resizable(false)
                .frame(
                    egui::Frame::new()
                        .fill(ui.visuals().panel_fill)
                        .inner_margin(egui::Margin::symmetric(0, 8)),
                )
                .show_separator_line(true)
                .show_inside(ui, |ui| {
                    ui.label(RichText::new("ПРЕДПРОСМОТР").size(10.0).color(MUTED));
                    egui::ScrollArea::vertical()
                        .id_salt("rhyme_preview_text")
                        .max_height(80.0)
                        .min_scrolled_height(0.0)
                        .show(ui, |ui| {
                            ui.label(RichText::new(&preview).size(17.0).color(ACCENT));
                        });
                    if ui
                        .add_sized(
                            [ui.available_width(), 36.0],
                            egui::Button::new("Заменить слово").fill(Color32::from_rgb(85, 66, 40)),
                        )
                        .clicked()
                    {
                        self.apply_candidate();
                    }
                });
        }
        let compact = self.compact_rhymes;
        let reveal = std::mem::take(&mut self.reveal_candidate);
        egui::ScrollArea::vertical()
            .id_salt(("rhyme_results", compact))
            .auto_shrink([false, false])
            .min_scrolled_height(0.0)
            .max_height(ui.available_height().max(1.0))
            .show(ui, |ui| {
                let mut render = |ui: &mut egui::Ui| {
                    for (index, candidate) in self.candidates.iter().enumerate() {
                        if self.closer_only && candidate.score < 120 {
                            continue;
                        }
                        let selected = self.chosen == Some(index);
                        let response = if compact {
                            // Lay out each complete word before flowing the buttons, so a
                            // narrow remainder never turns a word into a vertical column.
                            let width = (ui.max_rect().width() - 12.0).max(1.0);
                            let galley = ui.fonts(|fonts| {
                                fonts.layout(
                                    candidate.stressed.clone(),
                                    FontId::proportional(15.0),
                                    INK,
                                    width,
                                )
                            });
                            ui.add(
                                egui::Button::new(galley)
                                    .min_size(egui::vec2(24.0, 28.0))
                                    .selected(selected),
                            )
                        } else {
                            ui.add_sized(
                                [ui.available_width(), 32.0],
                                egui::Button::new(RichText::new(&candidate.stressed).size(17.0))
                                    .wrap()
                                    .selected(selected),
                            )
                        }
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text(format!(
                            "{} · {} слогов · {}",
                            candidate.stressed, candidate.syllables, candidate.part_of_speech
                        ));
                        if selected && reveal {
                            response.scroll_to_me(None);
                        }
                        if response.clicked() {
                            self.chosen = Some(index);
                            self.reveal_candidate = true;
                            ctx.request_repaint();
                        }
                    }
                };
                if compact {
                    ui.scope(|ui| {
                        ui.spacing_mut().button_padding = egui::vec2(6.0, 3.0);
                        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                        ui.horizontal_wrapped(&mut render);
                    });
                } else {
                    render(ui);
                }
            });
    }

    fn schedule_stress(&mut self) {
        self.stress_ticket += 1;
        self.stress_analysis = None;
        self.meter_analysis = None;
        self.stress_loading = false;
        self.stress_due = (self.stress_mode || self.meter_mode)
            .then(|| Instant::now() + Duration::from_millis(80));
    }

    fn start_stress(&mut self, ctx: &egui::Context) {
        self.stress_due = None;
        let ticket = self.stress_ticket;
        let text = self.text.clone();
        let marks = self.stress_marks.clone();
        let lexicon = self.lexicon.clone();
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        self.stress_loading = true;
        std::thread::spawn(move || {
            let analysis = stress::analyze(&text, &marks, lexicon.as_deref());
            let metrical = meter::analyze(&text, &analysis);
            let _ = tx.send(Background::Stress(
                ticket,
                Arc::new(analysis),
                Arc::new(metrical),
            ));
            ctx.request_repaint();
        });
    }

    fn set_word_stress(&mut self, start: usize, vowel: Option<usize>) {
        let Some(token) = self
            .stress_analysis
            .as_ref()
            .and_then(|analysis| {
                analysis
                    .tokens
                    .iter()
                    .find(|token| token.span.start == start)
            })
            .cloned()
        else {
            return;
        };
        if self.text.get(token.span.clone()) != Some(token.word.as_str())
            || vowel.is_some_and(|v| v >= token.vowels.len())
        {
            return;
        }
        if self
            .stress_marks
            .iter()
            .any(|mark| mark.start == start && mark.word == token.word && mark.vowel == vowel)
        {
            return;
        }
        let previous = self.snapshot();
        self.stress_marks.retain(|mark| mark.start != start);
        let mut word = token.word.clone();
        // An accent already present in the source must be replaced, not drawn twice.
        if word.contains('\u{301}') {
            word = vowel.map_or_else(|| stress::clean(&word), |v| stress::accented(&word, v));
            self.text.replace_range(token.span, &word);
            stress::rebase(&self.stress_text, &self.text, &mut self.stress_marks);
            meter::rebase(&self.stress_text, &self.text, &mut self.meter_overrides);
            self.stress_text = self.text.clone();
        }
        self.stress_marks.push(stress::Mark { start, word, vowel });
        let text_changed = previous.text != self.text;
        self.push_undo(previous);
        self.last_typing = None;
        if text_changed {
            self.after_document_change("Изменение ударения в тексте");
        } else {
            self.schedule_stress();
            self.schedule_search();
            self.autosave_due = Some(Instant::now() + Duration::from_millis(800));
            self.record_revision("Ручное ударение");
        }
        self.stress_selected = Some(start);
        self.set_status("Ударение сохранено в разметке. Ctrl+Z — отменить.", false);
    }

    fn reset_word_stress(&mut self, start: usize) {
        if !self.stress_marks.iter().any(|mark| mark.start == start) {
            return;
        }
        self.push_undo(self.snapshot());
        self.stress_marks.retain(|mark| mark.start != start);
        self.last_typing = None;
        self.schedule_stress();
        self.schedule_search();
        self.autosave_due = Some(Instant::now() + Duration::from_millis(800));
        self.record_revision("Автоматическое ударение");
    }

    fn open_stress_at_cursor(&mut self, ctx: &egui::Context) {
        let Some(id) = self.poem_editor_id else {
            return;
        };
        let Some(range) =
            egui::TextEdit::load_state(ctx, id).and_then(|state| state.cursor.char_range())
        else {
            return;
        };
        let byte = core::char_to_byte(&self.text, range.primary.index);
        if let Some(span) = stress::word_at(&self.text, byte) {
            if !stress::needs_mark(&self.text[span.clone()]) {
                return;
            }
            self.stress_selected = Some(span.start);
            self.stress_popup_pos = ctx.screen_rect().center() - egui::vec2(150.0, 80.0);
            self.stress_focus = true;
        }
    }

    fn word_stress_options(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        token: &stress::Token,
        vowels: &[usize],
    ) {
        ui.horizontal_wrapped(|ui| {
            for (index, &vowel) in vowels.iter().enumerate() {
                let response = stress_option(
                    ui,
                    &token.word,
                    vowel,
                    token.selected == Some(vowel),
                    token.choices.contains(&vowel),
                    false,
                );
                if self.stress_focus && index == 0 {
                    response.request_focus();
                    self.stress_focus = false;
                }
                if response.clicked() {
                    self.set_word_stress(token.span.start, Some(vowel));
                    self.close_stress_popup(ctx);
                }
            }
        });
    }

    fn close_stress_popup(&mut self, ctx: &egui::Context) {
        self.stress_selected = None;
        self.stress_focus = false;
        if let Some(id) = self.poem_editor_id {
            ctx.memory_mut(|memory| memory.request_focus(id));
        }
    }

    fn stress_window(&mut self, ctx: &egui::Context) {
        let Some(start) = self.stress_selected.filter(|_| self.stress_mode) else {
            return;
        };
        if self.pending_action.is_some() {
            return;
        }
        let token = self
            .stress_analysis
            .as_ref()
            .and_then(|analysis| {
                analysis
                    .tokens
                    .iter()
                    .find(|token| token.span.start == start)
            })
            .cloned();
        let mut open =
            !ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        egui::Window::new("Ударение в слове")
            .id(egui::Id::new("word_stress"))
            .open(&mut open)
            .fixed_pos(self.stress_popup_pos)
            .constrain_to(ctx.screen_rect())
            .default_width(310.0)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                let Some(token) = token else {
                    ui.spinner();
                    ui.label("Обновляем разметку…");
                    return;
                };
                let (_, manual_choice) = manual_stress_word(ui, &token.word, token.selected);
                if let Some(vowel) = manual_choice {
                    self.set_word_stress(start, Some(vowel));
                    self.close_stress_popup(ctx);
                }
                ui.label(match token.source {
                    stress::Source::Manual => "Выбрано вручную",
                    stress::Source::Dictionary => "Ударение из словаря",
                    stress::Source::Text => "Ударение уже записано в тексте",
                    stress::Source::Single => "Один слог. В строке слово может быть безударным.",
                    stress::Source::Yo => "Подсказка по букве ё — проверьте составные слова",
                    stress::Source::Ambiguous => "Несколько вариантов — выберите по смыслу",
                    stress::Source::Unknown => "Нет в словаре — выберите ударную гласную",
                });
                if token.word.contains('\u{301}') {
                    ui.label(
                        RichText::new("Выбор другой гласной заменит знак в исходном слове.")
                            .small()
                            .color(MUTED),
                    );
                }
                if token.choices.is_empty() {
                    ui.label("Словарных вариантов нет. Укажите ударение двойным кликом по гласной в слове сверху.");
                } else {
                    ui.label("Из словаря Rhymes:");
                    self.word_stress_options(ui, ctx, &token, &token.choices);
                    if token.selected.is_some_and(|vowel| !token.choices.contains(&vowel)) {
                        ui.label("Текущий ручной выбор отсутствует в словаре.");
                    }
                }
                ui.horizontal(|ui| {
                    if ui.button("Без ударения").clicked() {
                        self.set_word_stress(start, None);
                        self.close_stress_popup(ctx);
                    }
                    if ui
                        .add_enabled(
                            self.stress_marks.iter().any(|mark| mark.start == start),
                            egui::Button::new("Авто"),
                        )
                        .clicked()
                    {
                        self.reset_word_stress(start);
                        self.close_stress_popup(ctx);
                    }
                });
                ui.label(RichText::new("Двойной клик по гласной сверху — вручную · Esc — закрыть").small().color(MUTED));
            });
        if !open {
            self.stress_selected = None;
            if let Some(id) = self.poem_editor_id {
                ctx.memory_mut(|memory| memory.request_focus(id));
            }
        }
    }

    fn copy_stress(&mut self, ctx: &egui::Context) {
        let Some(analysis) = &self.stress_analysis else {
            return;
        };
        let selected = self
            .poem_editor_id
            .and_then(|id| egui::TextEdit::load_state(ctx, id))
            .and_then(|state| state.cursor.char_range())
            .filter(|range| range.primary.index != range.secondary.index)
            .map(|range| {
                core::char_to_byte(&self.text, range.primary.index.min(range.secondary.index))
                    ..core::char_to_byte(&self.text, range.primary.index.max(range.secondary.index))
            });
        let fragment = selected.is_some();
        let range = selected.unwrap_or(0..self.text.len());
        let text = stress::export_range(&self.text, analysis, range.clone());
        let unresolved = analysis
            .tokens
            .iter()
            .filter(|token| {
                token.span.start < range.end
                    && token.span.end > range.start
                    && stress::needs_mark(&token.word)
                    && matches!(
                        token.source,
                        stress::Source::Unknown | stress::Source::Ambiguous
                    )
            })
            .count();
        ctx.copy_text(text);
        let subject = if fragment {
            "Фрагмент"
        } else {
            "Текст"
        };
        self.set_status(
            if unresolved == 0 {
                format!("{subject} с ударениями скопирован.")
            } else {
                format!(
                    "{subject} скопирован. Осталось выбрать ударение в {} словах.",
                    unresolved
                )
            },
            false,
        );
    }

    fn set_meter_override(&mut self, index: usize, choice: meter::Choice, feet: Option<usize>) {
        let Some((start, guard)) = meter::anchor(&self.text, index, self.meter_scope) else {
            return;
        };
        let feet = if matches!(choice, meter::Choice::Regular(_)) {
            feet.map(|n| n.clamp(1, 32))
        } else {
            None
        };
        let old = self.meter_overrides.iter().find(|item| {
            item.scope == self.meter_scope && item.start == start && item.guard == guard
        });
        if old.is_none() && choice == meter::Choice::Auto {
            return;
        }
        if old.is_some_and(|item| item.choice == choice && item.feet == feet) {
            return;
        }
        self.push_undo(self.snapshot());
        self.last_typing = None;
        self.meter_overrides.retain(|item| {
            !(item.scope == self.meter_scope && item.start == start && item.guard == guard)
        });
        if choice != meter::Choice::Auto {
            self.meter_overrides.push(meter::Override {
                scope: self.meter_scope,
                start,
                guard,
                choice,
                feet,
            });
        }
        self.autosave_due = Some(Instant::now() + Duration::from_millis(800));
        self.record_revision("Ручная модель размера");
        self.set_status("Модель размера обновлена. Ctrl+Z — отменить.", false);
    }

    fn inspect_meter_syllable(&mut self, byte: usize, ctx: &egui::Context) {
        if let Some(word) = stress::word_at(&self.text, byte) {
            if !stress::needs_mark(&self.text[word.clone()]) {
                return;
            }
            self.stress_mode = true;
            self.stress_selected = Some(word.start);
            self.stress_popup_pos = ctx.screen_rect().center() - egui::vec2(150.0, 80.0);
            self.stress_focus = true;
            self.navigate_to(byte);
            self.autosave_due = Some(Instant::now() + Duration::from_millis(800));
        }
    }

    fn meter_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(analysis) = self.meter_analysis.clone() else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Анализируем размер…");
            });
            return;
        };
        let Some(line) = self
            .meter_line
            .and_then(|index| {
                analysis
                    .line(index)
                    .or_else(|| analysis.lines.iter().find(|line| line.index >= index))
            })
            .or_else(|| analysis.lines.first())
        else {
            ui.label("Добавьте стихотворные строки для анализа размера.");
            return;
        };
        let section = &analysis.sections[line.section];
        let own = meter::anchor(&self.text, line.index, self.meter_scope)
            .and_then(|(start, guard)| {
                self.meter_overrides.iter().find(|item| {
                    item.scope == self.meter_scope && item.start == start && item.guard == guard
                })
            })
            .cloned();
        let mut choice = own.as_ref().map_or(meter::Choice::Auto, |item| item.choice);
        let mut feet = own.as_ref().and_then(|item| item.feet);
        let old = (choice, feet);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(format!("Размер · строка {}", line.index + 1)).strong());
            egui::ComboBox::from_id_salt("meter_scope")
                .width(110.0)
                .selected_text(if self.meter_scope == meter::Scope::Section {
                    "Для секции"
                } else {
                    "Для строки"
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.meter_scope, meter::Scope::Section, "Для секции");
                    ui.selectable_value(&mut self.meter_scope, meter::Scope::Line, "Для строки");
                });
            egui::ComboBox::from_id_salt("meter_model")
                .width(170.0)
                .selected_text(choice.name())
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut choice, meter::Choice::Auto, "Авто");
                    for model in meter::Meter::ALL {
                        ui.selectable_value(
                            &mut choice,
                            meter::Choice::Regular(model),
                            format!("{}  {}", model.name(), model.pattern()),
                        );
                    }
                    ui.selectable_value(&mut choice, meter::Choice::Free, "Свободный / смешанный");
                });
            if let meter::Choice::Regular(model) = choice {
                let mut custom = feet.is_some();
                let mut count = feet
                    .unwrap_or_else(|| meter::fit(line, model, None).feet)
                    .clamp(1, 32);
                let toggled = ui
                    .checkbox(&mut custom, "Стоп")
                    .on_hover_text("Задать число стоп для выбранной области")
                    .changed();
                let changed = ui
                    .add_enabled(custom, egui::DragValue::new(&mut count).range(1..=32))
                    .changed();
                if toggled || changed {
                    feet = custom.then_some(count);
                }
            } else {
                feet = None;
            }
        });
        if (choice, feet) != old {
            self.set_meter_override(line.index, choice, feet);
        }
        let (detected, feet) = analysis.model(&self.text, line.index, &self.meter_overrides);
        let manual = meter::resolve(&self.text, line.index, &self.meter_overrides);
        let provenance = manual.map_or(
            if matches!(section.detected, meter::Detection::Regular(_)) {
                "авто секции"
            } else {
                "авто строки"
            },
            |item| {
                if item.scope == meter::Scope::Line {
                    "задано для строки"
                } else {
                    "задано для секции"
                }
            },
        );
        ui.label(
            RichText::new(format!(
                "Авто: секция «{}» — {} · строка — {}",
                section.name,
                section.detected.name(),
                line.detected.name()
            ))
            .small()
            .color(MUTED),
        )
        .on_hover_text(format!(
            "Заголовок секции: позиция {}. Автоматические оценки не переписывают ударения.",
            section.start
        ));
        let selected = if let meter::Detection::Regular(model) = detected {
            Some(meter::fit(line, model, feet))
        } else {
            None
        };
        if let Some(fit) = &selected {
            let estimate = if line.coverage < 0.55 || line.anchors < 2 {
                "недостаточно подтверждённых ударений"
            } else if line.uncertain_words > 0 {
                "есть неоднозначные слова"
            } else if fit.score >= 0.95 && line.anchors >= 3 {
                "хорошее соответствие"
            } else if fit.score >= 0.82 {
                "возможный вариант"
            } else {
                "заметные отклонения"
            };
            ui.label(
                RichText::new(format!(
                    "{} · стоп: {} · {} · {}",
                    fit.meter.name(),
                    fit.feet,
                    provenance,
                    estimate
                ))
                .color(ACCENT),
            );
            ui.label(RichText::new(format!("Вне схемы: {} · пропущенных сильных позиций: {} · неподтверждённых слов: {}{}",fit.extra.len(),fit.missing.len(),line.uncertain_words,if fit.length_mismatch {" · длина не соответствует числу стоп"}else{""})).small().color(MUTED))
                .on_hover_text("Пропуски сильных позиций бывают допустимым ритмическим вариантом. Совпадение с моделью — эвристическая оценка, не вероятность правильного ответа.");
        } else {
            ui.label(RichText::new(format!("{} · {}", detected.name(), provenance)).color(ACCENT));
        }
        let fallback = line.candidates.first();
        if let Some(fit) = selected.as_ref().or(fallback) {
            egui::ScrollArea::horizontal()
                .id_salt("meter_scansion")
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.add_sized([60.0, 24.0], egui::Label::new("Факт:"));
                        for (i, byte) in line.vowels.iter().enumerate() {
                            let uncertain = fit.uncertain[i] || fit.actual[i].is_none();
                            let symbol = if uncertain {
                                "?"
                            } else if fit.actual[i] == Some(true) {
                                "—"
                            } else {
                                "∪"
                            };
                            let color = if uncertain {
                                MUTED
                            } else if selected.is_some() && fit.extra.contains(&i) {
                                Color32::LIGHT_RED
                            } else if selected.is_some() && fit.missing.contains(&i) {
                                ACCENT
                            } else {
                                INK
                            };
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(symbol).monospace().size(16.0).color(color),
                                    )
                                    .frame(false)
                                    .min_size(egui::vec2(24.0, 24.0)),
                                )
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .on_hover_text(format!("Слог {} — выбрать ударение слова", i + 1))
                                .clicked()
                            {
                                self.inspect_meter_syllable(*byte, ctx);
                            }
                        }
                    });
                    if selected.is_some() {
                        ui.horizontal(|ui| {
                            ui.add_sized([60.0, 24.0], egui::Label::new("Схема:"));
                            for strong in &fit.expected {
                                ui.add_sized(
                                    [24.0, 24.0],
                                    egui::Label::new(
                                        RichText::new(if *strong { "—" } else { "∪" })
                                            .monospace()
                                            .size(16.0)
                                            .color(if *strong { ACCENT } else { MUTED }),
                                    ),
                                );
                            }
                        });
                    }
                });
        }
        ui.label(RichText::new("? — ударение не зафиксировано, включая контекстные односложные слова · нажмите слог для уточнения").small().color(MUTED));
        egui::CollapsingHeader::new("Другие модели").id_salt("meter_alternatives").show(ui,|ui| {
            ui.horizontal_wrapped(|ui| { for fit in &line.candidates {
                let label=if line.coverage>=0.55 && line.anchors>=2 {format!("{} · {}%",fit.meter.name(),(fit.score*100.0).round())}else{fit.meter.name().into()};
                if ui.button(label).on_hover_text("Сравнение с моделью для этой строки; нажмите, чтобы выбрать её для указанной области").clicked() {self.set_meter_override(line.index,meter::Choice::Regular(fit.meter),None);}
            }});
        });
        egui::CollapsingHeader::new("Строки секции")
            .id_salt("meter_section_lines")
            .show(ui, |ui| {
                for index in &section.lines {
                    let item = &analysis.lines[*index];
                    let (model, _) = analysis.model(&self.text, item.index, &self.meter_overrides);
                    if ui
                        .selectable_label(
                            item.index == line.index,
                            format!(
                                "{} · {} · {} слогов",
                                item.index + 1,
                                model.name(),
                                item.vowels.len()
                            ),
                        )
                        .clicked()
                    {
                        self.meter_line = Some(item.index);
                        self.navigate_to(item.start);
                    }
                }
            });
    }

    fn editor(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(20, 27, 39))
                    .inner_margin(28.0),
            )
            .show(ctx, |ui| {
                if self.meter_mode {
                    egui::TopBottomPanel::bottom("verse_meter_panel").resizable(true).default_height(270.0).min_height(170.0).max_height(350.0)
                        .frame(egui::Frame::new().fill(Color32::from_rgb(20,27,39)).inner_margin(egui::Margin::symmetric(0,8)))
                        .show_separator_line(true).show_inside(ui,|ui| {
                            egui::ScrollArea::vertical().id_salt("meter_panel_content").auto_shrink([false,false]).show(ui,|ui|self.meter_panel(ui,ctx));
                        });
                }
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("ВАШ ТЕКСТ").size(11.0).strong().color(MUTED));
                    ui.add_space(8.0);
                    if ui.add_enabled(!self.undo.is_empty(), egui::Button::new("Отменить").wrap_mode(egui::TextWrapMode::Extend))
                        .on_hover_text("Отменить последнюю правку · Ctrl+Z").clicked() {
                        self.undo();
                    }
                    if ui.add_enabled(!self.redo.is_empty(), egui::Button::new("Повторить").wrap_mode(egui::TextWrapMode::Extend))
                        .on_hover_text("Повторить отменённую правку · Ctrl+Y / Ctrl+Shift+Z").clicked() {
                        self.redo();
                    }
                    ui.add_space(8.0);
                    if ui.add(egui::Button::new("Форматировать").wrap_mode(egui::TextWrapMode::Extend)).on_hover_text("Убрать Markdown, вынести # название в верхнюю панель и оформить секции в [скобках]").clicked() {
                        self.format_document();
                    }
                    if ui.selectable_label(self.stress_mode, "Ударения").on_hover_text("Показывать ударения в стихотворении; нажмите на гласную, чтобы исправить").clicked() {
                        self.stress_mode = !self.stress_mode;
                        self.stress_selected = None;
                        self.schedule_stress();
                        self.autosave_due = Some(Instant::now() + Duration::from_millis(800));
                    }
                    if ui.selectable_label(self.meter_mode,"Размер").on_hover_text("Анализ размера выбранной строки и секции").clicked() {
                        self.meter_mode=!self.meter_mode;
                        self.schedule_stress();
                        self.autosave_due=Some(Instant::now()+Duration::from_millis(800));
                    }
                    if self.stress_mode && ui.add_enabled(self.stress_analysis.is_some(), egui::Button::new("Скопировать с ударениями").wrap_mode(egui::TextWrapMode::Extend)).on_hover_text("Копирует выделенный фрагмент, а без выделения — весь текст").clicked() { self.copy_stress(ctx); }
                    if !self.stress_mode && !self.meter_mode && ui.available_rect_before_wrap().width() > 300.0 {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new("Последние слова — точки для новых рифм").size(12.0).color(MUTED));
                        });
                    }
                });
                if self.stress_mode {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Подсвечены слова без выбранного ударения · клик по слову или Ctrl+Enter — варианты").small().color(MUTED));
                        if let Some(analysis) = &self.stress_analysis {
                            let pending = |source| analysis.tokens.iter().filter(|token| token.source == source && stress::needs_mark(&token.word)).count();
                            ui.label(RichText::new(format!("Неоднозначно: {} · нет в словаре: {}", pending(stress::Source::Ambiguous), pending(stress::Source::Unknown))).small().color(ACCENT));
                        } else { ui.spinner(); }
                    });
                }
                ui.add_space(24.0);
                let editor_id = ui.make_persistent_id("poem_text");
                self.poem_editor_id = Some(editor_id);
                if let Some(cursor) = self.requested_cursor.take() {
                    {
                        let mut state = egui::TextEdit::load_state(ctx, editor_id).unwrap_or_default();
                        state
                            .cursor
                            .set_char_range(Some(egui::text::CCursorRange::one(
                                egui::text::CCursor::new(cursor),
                            )));
                        state.store(ctx, editor_id);
                    }
                    ui.memory_mut(|memory| memory.request_focus(editor_id));
                }
                let previous = self.snapshot();
                if self.smoke_test && matches!(self.smoke_stage, 6 | 8 | 10) {
                    let mut state = egui::TextEdit::load_state(ctx, editor_id).unwrap_or_default();
                    let count = self.text.chars().count();
                    let range = if self.smoke_stage == 6 {
                        egui::text::CCursorRange::two(
                            egui::text::CCursor::new(0),
                            egui::text::CCursor::new(count),
                        )
                    } else {
                        let at = if self.smoke_stage == 10 { core::byte_to_char(&self.text,self.text.find("полями").unwrap()) } else { count };
                        egui::text::CCursorRange::one(egui::text::CCursor::new(at))
                    };
                    state.cursor.set_char_range(Some(range));
                    state.store(ctx, editor_id);
                    ui.memory_mut(|memory| memory.request_focus(editor_id));
                    let draft = if self.smoke_stage == 6 {
                        FORMAT_DRAFT
                    } else if self.smoke_stage == 8 {
                        "\n# Бридж\nТихо\n"
                    } else { "**тихими** " };
                    ctx.input_mut(|input| input.events.push(egui::Event::Paste(draft.into())));
                    self.smoke_stage += 1;
                }
                let mut pasted = false;
                if ui.memory(|memory| memory.has_focus(editor_id)) {
                    let range = egui::TextEdit::load_state(ctx, editor_id)
                        .and_then(|state| state.cursor.char_range())
                        .map(|range| range.primary.index.min(range.secondary.index)..range.primary.index.max(range.secondary.index));
                    let replaces_all = self.text.trim().is_empty() || range.as_ref()
                        .is_some_and(|range| range.start == 0 && range.end == self.text.chars().count());
                    let inline_position = range.as_ref().is_some_and(|range| {
                        let start = core::char_to_byte(&self.text, range.start);
                        let end = core::char_to_byte(&self.text, range.end);
                        !self.text[..start].rsplit('\n').next().unwrap_or("").trim().is_empty()
                            || !self.text[end..].split('\n').next().unwrap_or("").trim().is_empty()
                    });
                    let mut title = None;
                    ctx.input_mut(|input| {
                        for event in &mut input.events {
                            if let egui::Event::Paste(contents) = event {
                                let formatted = if inline_position && !contents.contains(['\n', '\r']) {
                                    formatter::FormattedPaste {text: formatter::format_inline(contents), title:None}
                                } else { formatter::format_paste(contents, replaces_all && title.is_none()) };
                                *contents = formatted.text;
                                if formatted.title.is_some() { title = formatted.title; }
                                pasted = true;
                            }
                        }
                    });
                    if let Some(title) = title { self.document_title = Some(title); }
                }
                let selected = self
                    .selection
                    .as_ref()
                    .map(|selection| selection.span.clone());
                let stress_highlights = self.stress_analysis.clone().filter(|_| self.stress_mode);
                let analyzed_text = &self.stress_text;
                let mut layouter = |ui: &egui::Ui, text: &str, wrap_width: f32| {
                    let mut job = layout_poem(text, selected.as_ref());
                    if text == analyzed_text { if let Some(analysis) = &stress_highlights { highlight_unresolved(&mut job, analysis); } }
                    job.wrap.max_width = wrap_width;
                    ui.fonts(|fonts| fonts.layout_job(job))
                };
                let mut clicked_position = None;
                let mut stress_hit = None;
                let mut changed = false;
                let scroll_output = egui::ScrollArea::vertical()
                    .id_salt("poem_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.horizontal_top(|ui| {
                        ui.add_space(26.0);
                        let output = egui::TextEdit::multiline(&mut self.text)
                            .id(editor_id)
                            .font(FontId::proportional(21.0))
                            .desired_width(f32::INFINITY)
                            .desired_rows(22)
                            .frame(false)
                            .hint_text("[Куплет 1]\nНачните писать или вставьте заготовку…")
                            .layouter(&mut layouter)
                            .show(ui);
                        if self.stress_mode && !output.response.changed() {
                            if let Some(analysis) = &self.stress_analysis {
                                stress_hit = paint_stresses(ui, &output.galley, output.galley_pos, &output.response, analysis);
                            }
                        }
                        if let Some(range)=output.cursor_range {
                            let byte=core::char_to_byte(&self.text,range.primary.ccursor.index);
                            if let Some(line)=core::parse_lines(&self.text).iter().find(|line|line.span.start<=byte && byte<=line.span.end) {
                                if self.meter_line!=Some(line.index) {self.meter_line=Some(line.index);ctx.request_repaint();}
                            }
                        }
                        let numbers = core::verse_numbers(&self.text);
                        // Track the section at the top reading edge, including wrapped lines.
                        // Include the first line's height: the scroll clip can start
                        // above the text padding even when its heading is at the top.
                        let reading_y = ui.clip_rect().top().max(output.galley_pos.y) + 32.0;
                        let cursor = output.galley.cursor_from_pos(egui::vec2(0.0, reading_y - output.galley_pos.y));
                        let byte = core::char_to_byte(&self.text, cursor.ccursor.index);
                        let section = core::parse_lines(&self.text).iter()
                            .take_while(|line| line.span.start <= byte)
                            .filter(|line| line.section.is_some()).last().map(|line| line.span.start);
                        if section != self.visible_section {
                            self.visible_section = section;
                            ctx.request_repaint();
                        }
                        let mut line = 0;
                        let mut first_row = true;
                        for row in &output.galley.rows {
                            if first_row {
                                if let Some(number) = numbers.get(line).copied().flatten() {
                                    let pos = egui::pos2(output.galley_pos.x - 16.0, output.galley_pos.y + row.rect.center().y);
                                    ui.painter().text(pos, egui::Align2::RIGHT_CENTER, number.to_string(), FontId::proportional(12.0), MUTED);
                                }
                            }
                            first_row = row.ends_with_newline;
                            if row.ends_with_newline { line += 1; }
                        }
                        if self.meter_mode && !output.response.changed() {
                            if let Some(analysis)=&self.meter_analysis {
                                if let Some(line)=self.meter_line.and_then(|index|analysis.line(index)).or_else(||analysis.lines.first()) {
                                    if let (meter::Detection::Regular(model),feet)=analysis.model(&self.text,line.index,&self.meter_overrides) {
                                        let fit=meter::fit(line,model,feet);
                                        paint_meter_deviations(ui,&output.galley,output.galley_pos,line,&fit);
                                    }
                                }
                            }
                        }
                        if let Some(cursor) = self.requested_scroll.take() {
                            let rect = output.galley.pos_from_ccursor(egui::text::CCursor::new(cursor))
                                .translate(output.galley_pos.to_vec2());
                            ui.scroll_to_rect(rect, Some(egui::Align::TOP));
                        }
                        changed = output.response.changed();
                        if output.response.clicked() && !output.response.double_clicked() {
                            if let Some(pos) = output.response.interact_pointer_pos() {
                                let cursor = output.galley.cursor_from_pos(pos - output.galley_pos);
                                clicked_position = Some(cursor.ccursor.index);
                            }
                        }
                        });
                    });
                self.editor_scroll = scroll_output.state.offset.y;
                if changed || self.document_title != previous.title {
                    let now = Instant::now();
                    let coalesce = !pasted && self
                        .last_typing
                        .is_some_and(|last| now.duration_since(last) < Duration::from_millis(650));
                    if !coalesce {
                        self.push_undo(previous);
                    } else {
                        self.redo.clear();
                    }
                    self.last_typing = if pasted { None } else { Some(now) };
                    self.after_document_change("Набор / вставка");
                }
                if let Some(character) = clicked_position {
                    self.last_typing = None;
                    if self.stress_mode {
                        if let Some((start, _vowel, pos)) = stress_hit {
                            self.stress_popup_pos = pos;
                            self.stress_selected = Some(start);
                        } else { self.stress_selected = None; }
                    } else { self.select_word(core::char_to_byte(&self.text, character)); }
                }
            });
    }

    fn dictionary_window(&mut self, ctx: &egui::Context) {
        if !self.show_dictionary {
            return;
        }
        let mut open = true;
        egui::Window::new("Локальный словарь").open(&mut open).default_width(520.0).collapsible(false).show(ctx, |ui| {
            ui.label("Укажите папку вашей копии Rhymes. Словарь читается только на этом компьютере.");
            ui.add_space(12.0);
            ui.add(egui::TextEdit::singleline(&mut self.dictionary_folder).desired_width(f32::INFINITY).hint_text("Папка Rhymes 3"));
            ui.horizontal(|ui| {
                if ui.button("Выбрать папку…").clicked() {
                    if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                        self.dictionary_folder = folder.to_string_lossy().into_owned();
                    }
                }
                if ui.add_enabled(!self.dictionary_loading, egui::Button::new("Подключить")).clicked() {
                    self.load_dictionary(ctx);
                }
            });
            ui.add_space(12.0);
            if self.dictionary_loading {
                ui.horizontal(|ui| { ui.spinner(); ui.label("Подготавливаем словарь…"); });
            } else if let Some(error) = &self.dictionary_error {
                ui.colored_label(Color32::LIGHT_RED, error);
            } else if let Some(lexicon) = &self.lexicon {
                ui.colored_label(Color32::from_rgb(127, 201, 173), format!("Подключено: {} словоформ", format_count(lexicon.entries.len())));
                ui.label(RichText::new(&lexicon.source).small().color(MUTED));
            }
            ui.add_space(12.0);
            ui.label(RichText::new("Источник словарных данных: Rhymes / Rifmovnik.ru. Исходные файлы остаются без изменений.").small().color(MUTED));
        });
        self.show_dictionary = open;
    }

    fn pending_window(&mut self, ctx: &egui::Context) {
        if self.pending_action.is_none() {
            return;
        }
        let mut decision = 0;
        egui::Window::new("Сохранить изменения?")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("В тексте есть несохранённые изменения.");
                if matches!(self.pending_action, Some(PendingAction::Restart)) {
                    ui.label("Без сохранения файл не изменится; текущие правки будут перенесены в новую копию редактора.");
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Сохранить").clicked() {
                        decision = 1;
                    }
                    if ui.button("Продолжить без сохранения").clicked() {
                        decision = 2;
                    }
                    if ui.button("Отмена").clicked() {
                        decision = 3;
                    }
                });
            });
        if decision == 1 && self.save(false) || decision == 2 {
            if let Some(action) = self.pending_action.take() {
                self.execute_action(action, ctx);
            }
        } else if decision == 3 {
            self.pending_action = None;
        }
    }

    fn smoke_file_saving(&mut self, ctx: &egui::Context) -> Result<(), String> {
        fn check(ok: bool, message: &str) -> Result<(), String> {
            if ok {
                Ok(())
            } else {
                Err(message.into())
            }
        }
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let original = dir.path().join("исходник.txt");
        storage::write_text(&original, "Первая строка\n")?;
        self.execute_action(PendingAction::Open(original.clone()), ctx);
        check(!self.is_dirty(), "opening a file marked it dirty")?;
        check(
            self.file_path.as_ref() == Some(&original),
            "opening lost the file path",
        )?;
        self.text.push_str("Правка\n");
        self.after_document_change("Проверка сохранения");
        check(self.is_dirty(), "edit was not marked dirty")?;
        check(self.save(false), "saving the opened file failed")?;
        check(
            storage::read_text(&original)? == self.text,
            "save did not overwrite the opened file",
        )?;
        self.document_title = Some("Название песни".into());
        check(self.save(false), "saving title metadata failed")?;

        let clean = self.snapshot();
        let session: storage::Session = serde_json::from_slice(
            &serde_json::to_vec(&self.recovery_session()).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        check(
            session.file_path.as_ref() == Some(&original),
            "fallback recovery lost file path",
        )?;
        self.restore_recovered_document(clean.clone(), session.file_path);
        check(!self.is_dirty(), "unchanged recovered file marked dirty")?;
        let mut draft = clean.clone();
        draft.text.push_str("Восстановленная правка\n");
        self.restore_recovered_document(draft, Some(original.clone()));
        check(self.is_dirty(), "real unsaved recovery edit was hidden")?;
        check(self.save(false), "saving recovered file failed")?;
        check(
            self.file_path.as_ref() == Some(&original),
            "recovered save changed the target",
        )?;
        check(
            storage::read_text(&original)? == self.text,
            "recovered save did not update original",
        )?;

        let original_text = self.text.clone();
        let copy = dir.path().join("копия.txt");
        check(self.save_to(copy.clone()), "save as failed")?;
        self.text.push_str("Правка копии\n");
        self.after_document_change("Проверка копии");
        check(self.save(false), "subsequent save of copy failed")?;
        check(
            self.file_path.as_ref() == Some(&copy),
            "save as target was not remembered",
        )?;
        check(storage::read_text(&copy)? == self.text, "copy not updated")?;
        check(
            storage::read_text(&original)? == original_text,
            "copy save overwrote original",
        )?;
        let blocked = dir.path().join("blocked");
        std::fs::write(&blocked, b"not a directory").map_err(|e| e.to_string())?;
        self.text.push_str("Несохранённая правка\n");
        check(
            !self.save_to(blocked.join("poem.txt")),
            "invalid save unexpectedly succeeded",
        )?;
        check(
            self.file_path.as_ref() == Some(&copy) && self.is_dirty(),
            "failed save changed target or cleared dirty state",
        )?;

        self.restore_recovered_document(clean.clone(), None);
        check(!self.is_dirty(), "recovered untitled draft marked dirty")?;
        self.text.push_str("Новая правка\n");
        check(self.is_dirty(), "new edit to recovered draft was hidden")?;
        let draft_path = dir.path().join("черновик.txt");
        check(
            self.save_to(draft_path.clone()),
            "first save of draft failed",
        )?;
        self.text.push_str("Ещё правка\n");
        check(self.save(false), "repeat save of draft failed")?;
        check(
            storage::read_text(&draft_path)? == self.text,
            "repeat save did not update draft file",
        )?;
        self.restore_recovered_document(clean, Some(dir.path().join("missing.txt")));
        check(self.is_dirty(), "missing file incorrectly marked saved")?;
        Ok(())
    }

    // Developer-only end-to-end command test. It uses a separate fixture,
    // never restores or writes the user's recovery file, and captures the GPU frame.
    fn smoke_update(&mut self, ctx: &egui::Context) {
        let directory = storage::app_directory().join("tmp/smoke-test");
        let finish = |ctx: &egui::Context, result: Result<(), String>| {
            let _ = std::fs::create_dir_all(&directory);
            let report = match result {
                Ok(()) => {
                    if std::env::args().any(|arg| arg == "--smoke-files") {
                        "PASS: opened/recovered file saving, save-as, repeated save and failed write handling".into()
                    } else {
                        "PASS: import, search, replace, undo, redo, preview and screenshot".into()
                    }
                }
                Err(e) => format!("FAIL: {e}"),
            };
            let _ = std::fs::write(directory.join("result.txt"), report);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        };
        if self.smoke_started.elapsed() > Duration::from_secs(60) {
            finish(ctx, Err("test timed out".into()));
            return;
        }
        if let Some(error) = &self.dictionary_error {
            finish(ctx, Err(error.clone()));
            return;
        }
        if self.smoke_stage == 0
            && self.lexicon.is_some()
            && !self.search_loading
            && self.search_due.is_none()
        {
            if let Some(index) = self.candidates.iter().position(|c| c.word == "багажа") {
                let before = self.text.clone();
                self.chosen = Some(index);
                self.apply_candidate();
                let replaced = before.replace("фуража", "багажа");
                if self.text != replaced {
                    finish(ctx, Err("replacement changed unexpected text".into()));
                    return;
                }
                self.undo();
                if self.text != before {
                    finish(ctx, Err("undo mismatch".into()));
                    return;
                }
                self.redo();
                if self.text != replaced {
                    finish(ctx, Err("redo mismatch".into()));
                    return;
                }
                self.undo();
                self.saved_text = Some(self.text.clone());
                self.select_word(self.text.find("фуража").unwrap());
                self.smoke_stage = 1;
            } else {
                finish(ctx, Err("багажа missing from candidates".into()));
            }
        } else if self.smoke_stage == 1 && !self.search_loading && self.search_due.is_none() {
            if std::env::args().any(|arg| arg == "--smoke-meter") {
                self.replace_document("[Куплет 1]\nМой дя́дя са́мых че́стных пра́вил,\nКогда́ не в шу́тку занемо́г,\n[Припев]\nМа́ма ма́ма ма́ма ма́ма\n".into(),None);
                self.meter_mode = true;
                self.stress_mode = false;
                self.meter_line = Some(1);
                self.navigate_to(self.text.find("Когда́").unwrap());
                self.schedule_stress();
                self.smoke_stage = 21;
                return;
            }
            if std::env::args().any(|arg| arg == "--smoke-stress-dictionary") {
                self.replace_document("[Куплет 1]\nИ выходит вместе с нами\n".into(), None);
                self.stress_mode = true;
                let start = self.text.find("выходит").unwrap();
                let analysis = stress::analyze(&self.text, &[], self.lexicon.as_deref());
                if analysis
                    .tokens
                    .iter()
                    .find(|token| token.span.start == start)
                    .unwrap()
                    .choices
                    != vec![0, 1]
                {
                    finish(
                        ctx,
                        Err("unexpected dictionary variants for выходит".into()),
                    );
                    return;
                }
                self.stress_analysis = Some(Arc::new(analysis));
                self.stress_selected = Some(start);
                self.stress_popup_pos = egui::pos2(530.0, 390.0);
                self.smoke_started = Instant::now();
                self.smoke_stage = 2;
                return;
            }
            if std::env::args().any(|arg| arg == "--smoke-stress") {
                self.replace_document("[Куплет 1]\nСтарый замок на горе,\nЁжик прячется в траве.\nФыжобряк тихонько дышит,\nОблачённый ветром слышит.\n".into(), None);
                self.stress_mode = true;
                self.schedule_stress();
                self.smoke_stage = 18;
                return;
            }

            if std::env::args().any(|arg| arg == "--smoke-rhymes-syllables") {
                self.replace_document("[Куплет 1]\nНе министр, не учёный,\nНе артист и не пилот —\nВ серый ватник облачённый\nТракторист Семён Петров.\n".into(), None);
                self.select_word(self.text.find("облачённый").unwrap());
                let line = self.selection.as_ref().unwrap().line;
                if self.syllable_count != 4 || core::parse_lines(&self.text)[line].syllables != 8 {
                    finish(ctx, Err("word and line syllable counts mismatch".into()));
                    return;
                }
                self.compact_rhymes = std::env::args().any(|arg| arg == "--smoke-rhymes-compact");
                self.smoke_stage = 5;
                return;
            }
            if std::env::args().any(|arg| arg == "--smoke-files") {
                let result = self.smoke_file_saving(ctx);
                finish(ctx, result);
                self.smoke_stage = 4;
                return;
            }
            if std::env::args().any(|arg| arg == "--smoke-navigation") {
                let text = format!("[Начало]\n{}\n[Проверка перехода]\nДлинная строка {}\nВторая строка\nТретья строка\nЧетвёртая строка\n\n{}", "Строка до секции\n".repeat(80), "со словами ".repeat(25), "Строка после секции\n".repeat(80));
                self.replace_document(text, None);
                self.navigate_to(self.text.find("[Проверка перехода]").unwrap());
                self.smoke_started = Instant::now();
                self.smoke_stage = 15;
                return;
            }
            if std::env::args().any(|arg| arg == "--smoke-history") {
                if let Some(history) = &self.history {
                    history.catalogue();
                }
                self.smoke_stage = 12;
                return;
            }
            if std::env::args().any(|arg| arg == "--smoke-formatter") {
                self.replace_document(FORMAT_DRAFT.into(), None);
                self.format_document();
                if self.text != FORMATTED_DRAFT
                    || self.document_title.as_deref() != Some("«Тракторист Семён Петров»")
                {
                    finish(ctx, Err("format button mismatch".into()));
                    return;
                }
                self.undo();
                if self.text != FORMAT_DRAFT || self.document_title.is_some() {
                    finish(ctx, Err("format button undo mismatch".into()));
                    return;
                }
                self.replace_document("Старый текст".into(), None);
                self.smoke_stage = 6;
                return;
            }
            if std::env::args().any(|arg| arg == "--smoke-stanzas") {
                self.replace_document("# Тракторист Семён Петров\n\n**Куплет 1**\nНа рассвете над полями\nПоднимается туман,\nИ выходит вместе с нами\nНа работу ветеран.\n\n[Припев]\nЗдесь другая строка\n".into(), None);
                let target = self.text.find("полями").unwrap();
                self.select_word(target);
                if self.anchor != "нами" {
                    finish(ctx, Err("wrong stanza partner".into()));
                    return;
                }
                self.anchor = "ветеран".into();
                self.anchor_line = Some(6);
                self.remember_anchor();
                self.select_word(self.text.find("туман").unwrap());
                self.select_word(target);
                if self.anchor != "ветеран" {
                    finish(ctx, Err("manual partner lost".into()));
                    return;
                }
                self.rhyme_links.clear();
                self.select_word(target);
                if self.anchor != "нами" {
                    finish(ctx, Err("auto partner not restored".into()));
                    return;
                }
                self.smoke_stage = 5;
                return;
            }
            self.chosen = self.candidates.iter().position(|c| c.word == "багажа");
            if std::env::args().any(|arg| arg == "--smoke-rhymes-compact") {
                self.compact_rhymes = true;
            }
            if std::env::args().any(|arg| arg == "--smoke-rhymes-list") {
                self.chosen = None;
            }
            if std::env::args().any(|arg| arg == "--smoke-updates") {
                self.show_updates = true;
                self.restart_available = true;
                self.update_message = "Обновление установлено.".into();
            }
            self.set_status("Проверено: словарь, поиск, замена, отмена и повтор.", false);
            self.smoke_started = Instant::now();
            self.smoke_stage = 2;
        } else if self.smoke_stage == 21 && self.meter_analysis.is_some() {
            let analysis = self.meter_analysis.clone().unwrap();
            if analysis.line(1).unwrap().detected != meter::Detection::Regular(meter::Meter::Iamb)
                || analysis.line(4).unwrap().detected
                    != meter::Detection::Regular(meter::Meter::Trochee)
            {
                finish(ctx, Err("meter auto classification mismatch".into()));
                return;
            }
            let before = self.text.clone();
            self.meter_scope = meter::Scope::Section;
            self.set_meter_override(2, meter::Choice::Regular(meter::Meter::Amphibrach), Some(3));
            self.meter_scope = meter::Scope::Line;
            self.set_meter_override(2, meter::Choice::Regular(meter::Meter::Iamb), Some(4));
            if analysis.model(&self.text, 2, &self.meter_overrides).0
                != meter::Detection::Regular(meter::Meter::Iamb)
            {
                finish(ctx, Err("line override did not take priority".into()));
                return;
            }
            self.undo();
            if analysis.model(&self.text, 2, &self.meter_overrides).0
                != meter::Detection::Regular(meter::Meter::Amphibrach)
            {
                finish(ctx, Err("meter undo did not restore section model".into()));
                return;
            }
            self.redo();
            if self.text != before || self.meter_overrides.len() != 2 {
                finish(ctx, Err("meter redo changed text or lost overrides".into()));
                return;
            }
            let session: storage::Session =
                serde_json::from_slice(&serde_json::to_vec(&self.recovery_session()).unwrap())
                    .unwrap();
            if session.meter_overrides != self.meter_overrides || !session.meter_mode {
                finish(ctx, Err("meter recovery roundtrip failed".into()));
                return;
            }
            self.stress_analysis = Some(Arc::new(stress::analyze(
                &self.text,
                &self.stress_marks,
                self.lexicon.as_deref(),
            )));
            self.meter_analysis = Some(Arc::new(meter::analyze(
                &self.text,
                self.stress_analysis.as_deref().unwrap(),
            )));
            self.meter_line = Some(2);
            self.set_status(
                "Проверено: размер, секции, приоритет строки, отмена/повтор и восстановление.",
                false,
            );
            self.smoke_started = Instant::now();
            self.smoke_stage = 2;
        } else if self.smoke_stage == 18 && self.stress_analysis.is_some() {
            let start = self.text.find("замок").unwrap();
            let before = self.text.clone();
            self.set_word_stress(start, Some(1));
            if self.text != before || self.stress_marks.len() != 1 {
                finish(ctx, Err("manual stress changed plain text".into()));
                return;
            }
            self.undo();
            if !self.stress_marks.is_empty() {
                finish(ctx, Err("stress undo failed".into()));
                return;
            }
            self.redo();
            if self.stress_marks.len() != 1 || self.stress_marks[0].vowel != Some(1) {
                finish(ctx, Err("stress redo failed".into()));
                return;
            }
            let session: storage::Session =
                serde_json::from_slice(&serde_json::to_vec(&self.recovery_session()).unwrap())
                    .unwrap();
            if session.stress_marks != self.stress_marks || !session.stress_mode {
                finish(ctx, Err("stress recovery roundtrip failed".into()));
                return;
            }
            self.history.as_ref().unwrap().catalogue();
            self.smoke_stage = 19;
        } else if self.smoke_stage == 19
            && self.stress_analysis.is_some()
            && !self.history_entries.is_empty()
        {
            let before_copy = self.text.clone();
            let before_path = self.file_path.clone();
            let before_dirty = self.is_dirty();
            self.copy_stress(ctx);
            let copied = ctx.output_mut(|output| {
                let index = output
                    .commands
                    .iter()
                    .position(|command| matches!(command, egui::OutputCommand::CopyText(_)))?;
                match output.commands.remove(index) {
                    egui::OutputCommand::CopyText(text) => Some(text),
                    _ => None,
                }
            });
            let exported = stress::export(&self.text, self.stress_analysis.as_deref().unwrap());
            if copied.as_deref() != Some(exported.as_str())
                || self.text != before_copy
                || self.file_path != before_path
                || self.is_dirty() != before_dirty
            {
                finish(
                    ctx,
                    Err("copy command changed document or copied wrong text".into()),
                );
                return;
            }
            let editor = self.poem_editor_id.unwrap();
            let original_state = egui::TextEdit::load_state(ctx, editor).unwrap();
            let start = core::byte_to_char(&self.text, self.text.find("замок").unwrap());
            let end = core::byte_to_char(
                &self.text,
                self.text.find("Фыжобряк").unwrap() + "Фыжобряк".len(),
            );
            for (a, b) in [(start, end), (end, start), (start, start)] {
                let mut state = original_state.clone();
                state
                    .cursor
                    .set_char_range(Some(egui::text::CCursorRange::two(
                        egui::text::CCursor::new(a),
                        egui::text::CCursor::new(b),
                    )));
                state.store(ctx, editor);
                self.copy_stress(ctx);
                let copied = ctx.output_mut(|output| {
                    let index = output
                        .commands
                        .iter()
                        .position(|command| matches!(command, egui::OutputCommand::CopyText(_)))?;
                    match output.commands.remove(index) {
                        egui::OutputCommand::CopyText(text) => Some(text),
                        _ => None,
                    }
                });
                let expected = if a == b {
                    exported.clone()
                } else {
                    stress::export_range(
                        &self.text,
                        self.stress_analysis.as_deref().unwrap(),
                        core::char_to_byte(&self.text, start)..core::char_to_byte(&self.text, end),
                    )
                };
                if copied.as_deref() != Some(expected.as_str())
                    || self.text != before_copy
                    || self.file_path != before_path
                    || self.is_dirty() != before_dirty
                {
                    finish(
                        ctx,
                        Err("selected/reverse/empty selection copy mismatch".into()),
                    );
                    return;
                }
            }
            original_state.store(ctx, editor);
            if !exported.contains("замо́к") || self.text.contains('\u{301}') {
                finish(ctx, Err("stress export/source mismatch".into()));
                return;
            }
            let Some(entry) = self.history_entries.iter().find(|entry| {
                entry.document == self.document_id && entry.reason == "Ручное ударение"
            }) else {
                return;
            };
            self.history_selected = Some(entry.id.clone());
            self.history.as_ref().unwrap().preview(&entry.id);
            self.smoke_stage = 20;
        } else if self.smoke_stage == 20 && self.history_preview.is_some() {
            if self.history_preview.as_ref().unwrap().stress_marks != self.stress_marks {
                finish(ctx, Err("revision lost stress marks".into()));
                return;
            }
            let before = self.snapshot();
            let path = self
                ._history_temp
                .as_ref()
                .unwrap()
                .path()
                .join("stress-fixture.txt");
            if !self.save_to(path.clone()) {
                finish(ctx, Err("saving stress fixture failed".into()));
                return;
            }
            self.execute_action(PendingAction::Open(path), ctx);
            if self.stress_marks != before.stress_marks || self.is_dirty() {
                finish(
                    ctx,
                    Err("reopening lost stress marks or marked file dirty".into()),
                );
                return;
            }
            self.restore_snapshot(DocumentState {
                text: "За́мок и сосед".into(),
                title: None,
                rhyme_links: vec![],
                stress_marks: vec![],
                meter_overrides: vec![],
            });
            self.stress_analysis = Some(Arc::new(stress::analyze(
                &self.text,
                &[],
                self.lexicon.as_deref(),
            )));
            self.set_word_stress(0, Some(1));
            if self.text != "Замо́к и сосед" {
                finish(ctx, Err("existing accent correction failed".into()));
                return;
            }
            self.undo();
            if self.text != "За́мок и сосед" || !self.stress_marks.is_empty() {
                finish(ctx, Err("existing accent undo failed".into()));
                return;
            }
            self.redo();
            self.text.insert_str(0, "И ");
            self.after_document_change("Проверка смещения метки");
            if self.stress_marks.first().map(|mark| mark.start) != Some("И ".len()) {
                finish(ctx, Err("stress mark did not shift after insertion".into()));
                return;
            }
            let mark = self.stress_marks[0].clone();
            self.text
                .replace_range(mark.start..mark.start + mark.word.len(), "Домок");
            self.after_document_change("Проверка сброса метки");
            if !self.stress_marks.is_empty() {
                finish(ctx, Err("changed word retained manual stress".into()));
                return;
            }
            self.restore_recovered_document(before, self.file_path.clone());
            self.undo.clear();
            self.redo.clear();
            let start = self.text.find("замок").unwrap();
            let editor = self.poem_editor_id.unwrap();
            let mut state = egui::TextEdit::load_state(ctx, editor).unwrap();
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(
                    egui::text::CCursor::new(core::byte_to_char(&self.text, start)),
                )));
            state.store(ctx, editor);
            self.open_stress_at_cursor(ctx);
            if self.stress_selected != Some(start) {
                finish(ctx, Err("keyboard stress selection failed".into()));
                return;
            }
            self.stress_popup_pos = egui::pos2(530.0, 440.0);
            self.set_status(
                "Проверено: ударения, ручной выбор, отмена, повтор, ревизии и копирование.",
                false,
            );
            self.smoke_started = Instant::now();
            self.smoke_stage = 2;
        } else if self.smoke_stage == 15
            && self.smoke_started.elapsed() > Duration::from_millis(700)
        {
            if self.editor_scroll < 1500.0 {
                finish(
                    ctx,
                    Err(format!(
                        "section navigation did not scroll: {}",
                        self.editor_scroll
                    )),
                );
                return;
            }
            self.navigate_to(0);
            self.smoke_started = Instant::now();
            self.smoke_stage = 16;
        } else if self.smoke_stage == 16
            && self.smoke_started.elapsed() > Duration::from_millis(700)
        {
            if self.editor_scroll > 1.0 {
                finish(
                    ctx,
                    Err(format!("navigation to top failed: {}", self.editor_scroll)),
                );
                return;
            }
            self.navigate_to(self.text.find("[Проверка перехода]").unwrap());
            self.smoke_started = Instant::now();
            self.smoke_stage = 17;
        } else if self.smoke_stage == 17
            && self.smoke_started.elapsed() > Duration::from_millis(700)
        {
            if self.editor_scroll < 1500.0 {
                finish(ctx, Err("second section navigation failed".into()));
                return;
            }
            if self.visible_section != self.text.find("[Проверка перехода]") {
                finish(
                    ctx,
                    Err(format!(
                        "visible section indicator: actual {:?}, expected {:?}, scroll {}",
                        self.visible_section,
                        self.text.find("[Проверка перехода]"),
                        self.editor_scroll
                    )),
                );
                return;
            }
            self.set_status(
                "Проверено: переход к секции, возврат наверх, номера строк и переносы.",
                false,
            );
            self.smoke_started = Instant::now();
            self.smoke_stage = 2;
        } else if self.smoke_stage == 12 && !self.history_entries.is_empty() {
            let Some(entry) = self
                .history_entries
                .iter()
                .find(|entry| entry.reason == "Замена рифмы")
            else {
                finish(ctx, Err("rhyme replacement missing from history".into()));
                return;
            };
            self.history_selected = Some(entry.id.clone());
            self.history.as_ref().unwrap().preview(&entry.id);
            self.smoke_stage = 13;
        } else if self.smoke_stage == 13 && self.history_preview.is_some() {
            let before = self.text.clone();
            self.restore_version(self.history_preview.clone().unwrap());
            if !self.text.contains("багажа") {
                finish(ctx, Err("revision restore mismatch".into()));
                return;
            }
            self.undo();
            if self.text != before {
                finish(ctx, Err("revision restore undo mismatch".into()));
                return;
            }
            self.redo();
            if !self.text.contains("багажа") {
                finish(ctx, Err("revision restore redo mismatch".into()));
                return;
            }
            self.history.as_ref().unwrap().catalogue();
            self.show_history = true;
            self.smoke_started = Instant::now();
            self.smoke_stage = 2;
        } else if self.smoke_stage == 7 {
            if self.text != FORMATTED_DRAFT
                || self.document_title.as_deref() != Some("«Тракторист Семён Петров»")
                || self.undo.len() != 1
            {
                finish(ctx, Err("paste formatting/title/history mismatch".into()));
                return;
            }
            self.undo();
            if self.text != "Старый текст" || self.document_title.is_some() {
                finish(ctx, Err("paste undo did not restore document title".into()));
                return;
            }
            self.redo();
            if self.text != FORMATTED_DRAFT
                || self.document_title.as_deref() != Some("«Тракторист Семён Петров»")
            {
                finish(ctx, Err("paste redo mismatch".into()));
                return;
            }
            let session = self.recovery_session();
            let restored: storage::Session =
                serde_json::from_str(&serde_json::to_string(&session).unwrap()).unwrap();
            if restored.text != self.text || restored.document_title != self.document_title {
                finish(ctx, Err("title recovery mismatch".into()));
                return;
            }
            self.requested_cursor = Some(self.text.chars().count());
            self.smoke_stage = 8;
        } else if self.smoke_stage == 9 {
            if !self.text.ends_with("\n[Бридж]\nТихо\n")
                || self.document_title.as_deref() != Some("«Тракторист Семён Петров»")
            {
                finish(
                    ctx,
                    Err("fragment paste renamed the document or retained Markdown".into()),
                );
                return;
            }
            self.undo();
            if self.text != FORMATTED_DRAFT {
                finish(ctx, Err("fragment paste was not one undo action".into()));
                return;
            }
            self.smoke_stage = 10;
        } else if self.smoke_stage == 11 {
            if !self.text.contains("над тихими полями")
                || self.text.contains("[тихими]")
                || self.document_title.as_deref() != Some("«Тракторист Семён Петров»")
            {
                finish(ctx, Err("inline paste was treated as a section".into()));
                return;
            }
            self.undo();
            if self.text != FORMATTED_DRAFT {
                finish(ctx, Err("inline paste undo mismatch".into()));
                return;
            }
            self.select_word(self.text.find("полями").unwrap());
            self.smoke_stage = 5;
        } else if self.smoke_stage == 5 && !self.search_loading && self.search_due.is_none() {
            self.set_status(
                "Проверено: Markdown, строфы, опора «нами», ручной выбор и возврат в авто.",
                false,
            );
            if std::env::args().any(|arg| arg == "--smoke-formatter") {
                self.set_status(
                    "Проверено: вставка Markdown, название документа, отмена и повтор.",
                    false,
                );
            }
            self.smoke_started = Instant::now();
            self.smoke_stage = 2;
        } else if self.smoke_stage == 2
            && (!self.stress_mode || self.stress_analysis.is_some())
            && self.smoke_started.elapsed() > Duration::from_millis(400)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            self.smoke_stage = 3;
        } else if self.smoke_stage == 3 {
            let screenshot = ctx.input(|input| {
                input.events.iter().find_map(|event| {
                    if let egui::Event::Screenshot { image, .. } = event {
                        Some(image.clone())
                    } else {
                        None
                    }
                })
            });
            if let Some(screenshot) = screenshot {
                let _ = std::fs::create_dir_all(&directory);
                let pixels: Vec<u8> = screenshot
                    .pixels
                    .iter()
                    .flat_map(|pixel| pixel.to_array())
                    .collect();
                let result = image::save_buffer(
                    directory.join("prototype.png"),
                    &pixels,
                    screenshot.size[0] as u32,
                    screenshot.size[1] as u32,
                    image::ColorType::Rgba8,
                )
                .map_err(|e| e.to_string());
                finish(ctx, result);
                self.smoke_stage = 4;
            }
        }
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

impl eframe::App for PoetApp {
    fn persist_egui_memory(&self) -> bool {
        false
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.restart_frame(ctx) {
            return;
        }
        if self.smoke_test
            && std::env::args()
                .any(|arg| arg == "--smoke-restart" || arg == "--smoke-restart-child")
        {
            self.smoke_restart(ctx);
            return;
        }
        self.receive_background();
        self.receive_history();
        let open_stress = self.stress_mode
            && self.pending_action.is_none()
            && ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
        if self.stress_mode
            && self.stress_selected.is_some()
            && self.pending_action.is_none()
            && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.stress_selected = None;
            self.stress_focus = false;
            if let Some(id) = self.poem_editor_id {
                ctx.memory_mut(|memory| memory.request_focus(id));
            }
        }
        if self.stress_due.is_some_and(|due| Instant::now() >= due) {
            self.start_stress(ctx);
        }
        if ctx.input(|input| input.viewport().close_requested())
            && !self.allow_close
            && !self.smoke_test
            && self.is_dirty()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.pending_action = Some(PendingAction::Close);
        }
        let (undo, redo, save, open) = ctx.input_mut(|input| {
            (
                input.consume_key(egui::Modifiers::COMMAND, egui::Key::Z),
                input.consume_key(egui::Modifiers::COMMAND, egui::Key::Y)
                    || input.consume_key(
                        egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                        egui::Key::Z,
                    ),
                input.consume_key(egui::Modifiers::COMMAND, egui::Key::S),
                input.consume_key(egui::Modifiers::COMMAND, egui::Key::O),
            )
        });
        if self.pending_action.is_none() {
            if undo {
                self.undo();
            }
            if redo {
                self.redo();
            }
            if save {
                self.save(false);
            }
            if open {
                self.open(ctx);
            }
        }
        if self.search_due.is_some_and(|due| Instant::now() >= due) {
            self.start_search(ctx);
        }
        if !self.smoke_test && self.autosave_due.is_some_and(|due| Instant::now() >= due) {
            self.autosave_due = None;
            if let Err(error) = storage::save_recovery(&self.recovery_session()) {
                self.set_status(
                    format!("Не удалось сохранить восстановление: {error}"),
                    true,
                );
            }
        }
        self.toolbar(ctx);
        egui::TopBottomPanel::bottom("status")
            .exact_height(34.0)
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.add_space(10.0);
                    ui.label(RichText::new(&self.status).size(12.0).color(
                        if self.status_is_error {
                            Color32::LIGHT_RED
                        } else {
                            MUTED
                        },
                    ));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(10.0);
                        let history_status = if self.history_error.is_some() {
                            "Ошибка истории"
                        } else if self.history.as_ref().is_some_and(|h| h.pending > 0) {
                            "Запись истории…"
                        } else {
                            "История сохранена"
                        };
                        ui.label(RichText::new(history_status).size(11.0).color(
                            if self.history_error.is_some() {
                                Color32::LIGHT_RED
                            } else {
                                MUTED
                            },
                        ))
                        .on_hover_text(
                            self.history_error
                                .as_deref()
                                .unwrap_or("Локальные ревизии · кнопка «История»"),
                        );
                        if self.dictionary_loading {
                            ui.spinner();
                        } else if let Some(lexicon) = &self.lexicon {
                            ui.label(
                                RichText::new(format!(
                                    "Rhymes · {} словоформ",
                                    format_count(lexicon.entries.len())
                                ))
                                .size(11.0)
                                .color(MUTED),
                            );
                        }
                    });
                });
            });
        self.outline(ctx);
        self.rhyme_panel(ctx);
        self.editor(ctx);
        if open_stress {
            self.open_stress_at_cursor(ctx);
        }
        self.dictionary_window(ctx);
        self.updates_window(ctx);
        if self.restart_state.is_some() {
            ctx.request_repaint();
            return;
        }
        self.history_window(ctx);
        self.stress_window(ctx);
        self.pending_window(ctx);
        if self.smoke_test {
            self.smoke_update(ctx);
        }
        if self.stress_due.is_some()
            || self.stress_loading
            || self.search_due.is_some()
            || self.autosave_due.is_some()
            || self.dictionary_loading
            || self.search_loading
        {
            ctx.request_repaint_after(Duration::from_millis(if self.search_due.is_some() {
                25
            } else {
                80
            }));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(history) = &mut self.history {
            history.shutdown();
        }
        if self.smoke_test || self.restart_committed || self.incoming_restart.is_some() {
            return;
        }
        let _ = storage::save_recovery(&self.recovery_session());
    }
}

fn paint_meter_deviations(
    ui: &egui::Ui,
    galley: &egui::Galley,
    origin: egui::Pos2,
    line: &meter::Line,
    fit: &meter::Fit,
) {
    let mut byte = 0;
    for row in &galley.rows {
        if !ui
            .clip_rect()
            .intersects(row.rect.translate(origin.to_vec2()))
        {
            byte += row
                .glyphs
                .iter()
                .map(|glyph| glyph.chr.len_utf8())
                .sum::<usize>()
                + usize::from(row.ends_with_newline);
            continue;
        }
        for glyph in &row.glyphs {
            if let Ok(index) = line.vowels.binary_search(&byte) {
                if !fit.uncertain[index] {
                    let color = if fit.extra.contains(&index) {
                        Some(Color32::LIGHT_RED)
                    } else if fit.missing.contains(&index) {
                        Some(ACCENT)
                    } else {
                        None
                    };
                    if let Some(color) = color {
                        let y = origin.y + row.rect.bottom() - 2.0;
                        ui.painter().line_segment(
                            [
                                egui::pos2(origin.x + glyph.pos.x, y),
                                egui::pos2(origin.x + glyph.pos.x + glyph.advance_width, y),
                            ],
                            Stroke::new(2.0_f32, color),
                        );
                    }
                }
            }
            byte += glyph.chr.len_utf8();
        }
        byte += usize::from(row.ends_with_newline);
    }
}

fn saved_anchor_stress(
    text: &str,
    line: Option<usize>,
    anchor: &str,
    marks: &[stress::Mark],
) -> Option<usize> {
    let line = line?;
    if marks.is_empty() || core::explicit_stress(anchor).is_some() {
        return None;
    }
    let lines = core::parse_lines(text);
    let span = lines.get(line)?.last_word.as_ref()?;
    if &text[span.clone()] != anchor {
        return None;
    }
    marks
        .iter()
        .find(|mark| mark.start == span.start && mark.word == anchor)
        .and_then(|mark| mark.vowel)
        .filter(|vowel| *vowel < core::syllables(anchor))
}

// One readable word with per-vowel hit testing; a single click never changes it.
fn manual_stress_word(
    ui: &mut egui::Ui,
    word: &str,
    selected: Option<usize>,
) -> (egui::Response, Option<usize>) {
    let clean = stress::clean(word);
    let galley = ui.fonts(|fonts| fonts.layout_no_wrap(clean, FontId::proportional(20.0), INK));
    let (rect, response) = ui.allocate_exact_size(galley.size(), egui::Sense::click());
    ui.painter().galley(rect.min, galley.clone(), INK);
    let pointer = ui
        .ctx()
        .input(|input| input.pointer.hover_pos())
        .filter(|_| response.hovered());
    let mut vowel = 0;
    let mut choice = None;
    for row in &galley.rows {
        for glyph in &row.glyphs {
            if !stress::is_vowel(glyph.chr) {
                continue;
            }
            let bounds = glyph.logical_rect().translate(rect.min.to_vec2());
            if selected == Some(vowel) {
                let x = bounds.center().x;
                let y = rect.top() + row.rect.top();
                ui.painter().line_segment(
                    [egui::pos2(x - 1.0, y + 5.0), egui::pos2(x + 2.0, y + 1.0)],
                    Stroke::new(1.5_f32, ACCENT),
                );
            }
            if pointer.is_some_and(|point| bounds.contains(point)) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                ui.painter().line_segment(
                    [bounds.left_bottom(), bounds.right_bottom()],
                    Stroke::new(1.0_f32, ACCENT),
                );
                response
                    .clone()
                    .on_hover_text("Двойной клик — поставить ударение на эту гласную вручную");
                if response.double_clicked() {
                    choice = Some(vowel);
                }
            }
            vowel += 1;
        }
    }
    (response, choice)
}

// egui does not shape combining accents consistently in all fonts. Render the
// clean word and draw the accent over its vowel, as in the poem itself.
fn stress_option(
    ui: &mut egui::Ui,
    word: &str,
    vowel: usize,
    selected: bool,
    dictionary: bool,
    small: bool,
) -> egui::Response {
    let clean = stress::clean(word);
    let font = if small {
        egui::TextStyle::Button.resolve(ui.style())
    } else {
        FontId::proportional(18.0)
    };
    let color = ui.visuals().text_color();
    let mut job = egui::text::LayoutJob::default();
    job.append(
        &clean,
        0.0,
        TextFormat {
            font_id: font,
            color,
            underline: if dictionary {
                Stroke::new(1.0_f32, color)
            } else {
                Stroke::NONE
            },
            ..Default::default()
        },
    );
    let galley = ui.fonts(|fonts| fonts.layout_job(job));
    let button = egui::Button::new(galley.clone()).selected(selected);
    let response = ui.add(if small { button.small() } else { button });
    if let Some((character, _)) = clean
        .chars()
        .enumerate()
        .filter(|(_, c)| stress::is_vowel(*c))
        .nth(vowel)
    {
        if let Some(row) = galley.rows.first() {
            if let Some(glyph) = row.glyphs.get(character) {
                let origin = response.rect.center() - galley.size() * 0.5;
                let x = origin.x + glyph.pos.x + glyph.advance_width * 0.5;
                let y = origin.y + row.rect.top();
                ui.painter().line_segment(
                    [egui::pos2(x - 1.0, y + 5.0), egui::pos2(x + 2.0, y + 1.0)],
                    Stroke::new(1.5_f32, color),
                );
            }
        }
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Button,
            ui.is_enabled(),
            selected,
            stress::accented(word, vowel),
        )
    });
    response.on_hover_text(format!("Ударение на {}-й слог", vowel + 1))
}

fn paint_stresses(
    ui: &egui::Ui,
    galley: &egui::Galley,
    origin: egui::Pos2,
    response: &egui::Response,
    analysis: &stress::Analysis,
) -> Option<(usize, Option<usize>, egui::Pos2)> {
    let manual = Color32::from_rgb(113, 216, 199);
    let pointer = ui
        .ctx()
        .input(|input| input.pointer.hover_pos())
        .filter(|_| response.hovered());
    let mut hit = None;
    let mut byte = 0;
    let mut token_index = 0;
    for row in &galley.rows {
        if !ui
            .clip_rect()
            .intersects(row.rect.translate(origin.to_vec2()))
        {
            byte += row
                .glyphs
                .iter()
                .map(|glyph| glyph.chr.len_utf8())
                .sum::<usize>()
                + usize::from(row.ends_with_newline);
            continue;
        }
        for glyph in &row.glyphs {
            while analysis
                .tokens
                .get(token_index)
                .is_some_and(|token| token.span.end <= byte)
            {
                token_index += 1;
            }
            if let Some(token) = analysis
                .tokens
                .get(token_index)
                .filter(|token| token.span.contains(&byte) && stress::needs_mark(&token.word))
            {
                let rect = glyph.logical_rect().translate(origin.to_vec2());
                let vowel = token
                    .vowels
                    .iter()
                    .position(|offset| *offset == byte - token.span.start);
                let x = origin.x + glyph.pos.x + glyph.advance_width * 0.5;
                if vowel.is_some()
                    && vowel == token.selected
                    && !token.word.contains('\u{301}')
                    && !matches!(glyph.chr, 'ё' | 'Ё')
                {
                    let y = origin.y + row.rect.top();
                    let color = if token.source == stress::Source::Manual {
                        manual
                    } else {
                        ACCENT
                    };
                    ui.painter().line_segment(
                        [egui::pos2(x - 1.0, y + 6.0), egui::pos2(x + 2.0, y + 2.0)],
                        Stroke::new(1.5_f32, color),
                    );
                }
                let y = origin.y + row.rect.bottom() - 3.0;
                if token.source == stress::Source::Manual && token.selected.is_none() {
                    ui.painter().line_segment(
                        [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                        Stroke::new(1.0_f32, manual),
                    );
                }
                if pointer.is_some_and(|pos| rect.contains(pos)) {
                    hit = Some((
                        token.span.start,
                        if matches!(
                            token.source,
                            stress::Source::Unknown | stress::Source::Ambiguous
                        ) {
                            None
                        } else {
                            vowel
                        },
                        rect.left_bottom() + egui::vec2(-40.0, 10.0),
                    ));
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    response
                        .clone()
                        .on_hover_text("Нажмите, чтобы выбрать вариант ударения");
                }
            }
            byte += glyph.chr.len_utf8();
        }
        byte += usize::from(row.ends_with_newline);
    }
    hit
}

fn outline_link(ui: &mut egui::Ui, title: &str, active: bool, line: usize) -> egui::Response {
    let response = ui
        .scope(|ui| {
            let visuals = ui.visuals_mut();
            visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
            visuals.widgets.inactive.bg_stroke = Stroke::NONE;
            visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(45, 63, 84);
            visuals.widgets.hovered.bg_stroke =
                Stroke::new(1.0_f32, Color32::from_rgb(113, 140, 171));
            visuals.widgets.active.weak_bg_fill = Color32::from_rgb(58, 77, 100);
            visuals.selection.bg_fill = Color32::from_rgb(64, 53, 40);
            visuals.selection.stroke = Stroke::NONE;
            let text = RichText::new(title).color(if active {
                ACCENT
            } else {
                Color32::from_rgb(174, 204, 234)
            });
            let text = if active { text.strong() } else { text };
            ui.add_sized(
                [ui.available_width(), 36.0],
                egui::Button::new(text).wrap().selected(active),
            )
        })
        .inner
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(format!("Перейти к секции «{title}» · строка {line}"));
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Button,
            ui.is_enabled(),
            active,
            format!("{title}, строка {line}"),
        )
    });
    if active {
        let rect = egui::Rect::from_min_max(
            response.rect.left_top() + egui::vec2(1.0, 5.0),
            response.rect.left_bottom() + egui::vec2(4.0, -5.0),
        );
        ui.painter().rect_filled(rect, 1.0, ACCENT);
    }
    if response.has_focus() {
        ui.painter().rect_stroke(
            response.rect.shrink(1.0),
            4.0,
            Stroke::new(2.0_f32, ACCENT),
            egui::StrokeKind::Inside,
        );
    }
    response
}

fn configure_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    let mut fonts = egui::FontDefinitions::default();
    if let Ok(bytes) = std::fs::read("C:/Windows/Fonts/segoeui.ttf") {
        fonts
            .font_data
            .insert("segoe".into(), egui::FontData::from_owned(bytes).into());
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "segoe".into());
        ctx.set_fonts(fonts);
    }
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = Color32::from_rgb(25, 34, 48);
    visuals.window_fill = Color32::from_rgb(29, 39, 54);
    visuals.extreme_bg_color = Color32::from_rgb(17, 24, 35);
    visuals.faint_bg_color = Color32::from_rgb(33, 45, 62);
    visuals.override_text_color = Some(INK);
    visuals.selection.bg_fill = Color32::from_rgb(75, 66, 52);
    visuals.selection.stroke = Stroke::new(1.0_f32, ACCENT);
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(36, 47, 64);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(53, 65, 84);
    visuals.widgets.active.bg_fill = Color32::from_rgb(67, 76, 94);
    ctx.set_visuals_of(egui::Theme::Dark, visuals);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        style.spacing.item_spacing = egui::vec2(8.0, 7.0);
        style.spacing.button_padding = egui::vec2(12.0, 7.0);
        style
            .text_styles
            .insert(egui::TextStyle::Body, FontId::proportional(15.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, FontId::proportional(14.0));
    });
}

// Split existing text formats so unresolved words are marked even at line endings.
// Token/section ranges stay in original UTF-8 coordinates and are traversed in order.
fn highlight_unresolved(job: &mut egui::text::LayoutJob, analysis: &stress::Analysis) {
    let unresolved: Vec<_> = analysis
        .tokens
        .iter()
        .filter(|token| {
            stress::needs_mark(&token.word)
                && matches!(
                    token.source,
                    stress::Source::Unknown | stress::Source::Ambiguous
                )
        })
        .collect();
    let mut index = 0;
    for section in std::mem::take(&mut job.sections) {
        let mut at = section.byte_range.start;
        while at < section.byte_range.end {
            while unresolved
                .get(index)
                .is_some_and(|token| token.span.end <= at)
            {
                index += 1;
            }
            let token = unresolved
                .get(index)
                .filter(|token| token.span.start < section.byte_range.end);
            let (end, marked) = match token {
                Some(token) if at < token.span.start => (token.span.start, false),
                Some(token) => (token.span.end.min(section.byte_range.end), true),
                None => (section.byte_range.end, false),
            };
            let mut fragment = section.clone();
            fragment.byte_range = at..end;
            if marked {
                fragment.format.color = Color32::from_rgb(250, 166, 151);
                fragment.format.background = Color32::from_rgb(61, 38, 43);
                fragment.format.underline = Stroke::new(1.0_f32, Color32::from_rgb(196, 111, 103));
            }
            job.sections.push(fragment);
            at = end;
        }
    }
}

fn layout_poem(text: &str, selected: Option<&Range<usize>>) -> egui::text::LayoutJob {
    let mut highlights = Vec::new();
    for line in core::parse_lines(text) {
        if line.section.is_some() {
            highlights.push((line.span, true));
        } else if let Some(span) = line.last_word {
            highlights.push((span, false));
        }
    }
    let normal = TextFormat {
        font_id: FontId::proportional(21.0),
        color: INK,
        line_height: Some(32.0),
        ..Default::default()
    };
    let mut job = egui::text::LayoutJob::default();
    let mut at = 0;
    for (span, header) in highlights {
        if span.start < at || span.end > text.len() {
            continue;
        }
        job.append(&text[at..span.start], 0.0, normal.clone());
        let mut format = normal.clone();
        if header {
            format.color = Color32::from_rgb(131, 185, 185);
            format.font_id = FontId::proportional(17.0);
        } else {
            format.color = ACCENT;
            format.underline = Stroke::new(1.0_f32, Color32::from_rgb(111, 91, 63));
            if selected == Some(&span) {
                format.background = Color32::from_rgb(69, 57, 42);
            }
        }
        job.append(&text[span.clone()], 0.0, format);
        at = span.end;
    }
    job.append(&text[at..], 0.0, normal);
    job
}

// Gregorian calendar conversion; dates in history are explicitly displayed in UTC.
fn revision_date(seconds: u64) -> String {
    let days = (seconds / 86400) as i64 + 719468;
    let era = days / 146097;
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!(
        "{day:02}.{month:02}.{year} {:02}:{:02}:{:02} UTC",
        seconds / 3600 % 24,
        seconds / 60 % 60,
        seconds % 60
    )
}

fn format_count(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod navigation_tests {
    use super::*;

    #[test]
    fn rhyme_search_uses_the_saved_stress_of_the_anchor_occurrence() {
        let text = "Я пил из кружки.\nМастер ведёт кружки.\nИ две старушки.";
        let marks = [stress::Mark {
            start: text.find("кружки").unwrap(),
            word: "кружки".into(),
            vowel: Some(0),
        }];
        let lexicon = Lexicon::from_entries(
            [("кружки", 0), ("кружки", 1), ("ватрушки", 1), ("береги", 2)]
                .into_iter()
                .map(|(word, stress)| crate::lexicon::Entry {
                    word: word.into(),
                    stress,
                    part_of_speech: String::new(),
                })
                .collect(),
            "fixture".into(),
        );
        let vowel = saved_anchor_stress(text, Some(0), "кружки", &marks).unwrap();
        let anchor = core::with_stress("кружки", vowel);
        let candidates = core::search_rhymes(&lexicon, &anchor, &core::SearchFilter::default(), 10);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].word, "ватрушки");

        assert_eq!(saved_anchor_stress(text, Some(1), "кружки", &marks), None);
        assert_eq!(saved_anchor_stress(text, None, "кружки", &marks), None);
        assert_eq!(saved_anchor_stress(text, Some(0), "кружки́", &marks), None);
    }

    #[test]
    fn manual_stress_requires_double_click_on_a_vowel() {
        for (character, expected) in [(5, Some(2)), (2, None)] {
            let ctx = egui::Context::default();
            let mut time = 0.0;
            let mut run = |events: Vec<egui::Event>| {
                time += 0.016;
                let mut target = egui::Pos2::ZERO;
                let mut choice = None;
                let output = ctx.run(
                    egui::RawInput {
                        time: Some(time),
                        events,
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(500.0, 300.0),
                        )),
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            let (response, selected) = manual_stress_word(ui, "выходит", None);
                            choice = selected;
                            let galley = ui.fonts(|fonts| {
                                fonts.layout_no_wrap(
                                    "выходит".into(),
                                    FontId::proportional(20.0),
                                    INK,
                                )
                            });
                            target = galley.rows[0].glyphs[character].logical_rect().center()
                                + response.rect.min.to_vec2();
                        });
                    },
                );
                (target, choice, output.platform_output.cursor_icon)
            };
            run(vec![]);
            let (target, _, _) = run(vec![]);
            let (_, choice, cursor) = run(vec![egui::Event::PointerMoved(target)]);
            assert!(choice.is_none());
            if expected.is_some() {
                assert_eq!(cursor, egui::CursorIcon::PointingHand);
            }
            for click in 0..2 {
                for pressed in [true, false] {
                    let (_, choice, _) = run(vec![egui::Event::PointerButton {
                        pos: target,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    }]);
                    if click == 0 || pressed {
                        assert!(choice.is_none());
                    } else {
                        assert_eq!(choice, expected);
                    }
                }
            }
        }
    }

    #[test]
    fn unresolved_highlights_preserve_unicode_and_cover_line_endings() {
        let text = "[Куплет]\nЗамок и Фыжобряк\nЁлочка дом\n";
        let analysis = stress::analyze(text, &[], None);
        let mut job = layout_poem(text, None);
        highlight_unresolved(&mut job, &analysis);
        assert_eq!(job.text, text);
        let rebuilt: String = job
            .sections
            .iter()
            .map(|section| &text[section.byte_range.clone()])
            .collect();
        assert_eq!(rebuilt, text);
        for word in ["Замок", "Фыжобряк"] {
            let start = text.find(word).unwrap();
            assert!(job
                .sections
                .iter()
                .any(|section| section.byte_range.contains(&start)
                    && section.format.background == Color32::from_rgb(61, 38, 43)));
        }
        for word in ["Ёлочка", "дом", "и"] {
            let start = text.find(word).unwrap();
            assert!(!job
                .sections
                .iter()
                .any(|section| section.byte_range.contains(&start)
                    && section.format.background == Color32::from_rgb(61, 38, 43)));
        }
    }

    #[test]
    fn stress_hit_test_matches_wrapped_cyrillic_vowel_coordinates() {
        let ctx = egui::Context::default();
        let original = "[Куплет]\nЭта длинная строка переносится: за́мок и замок.";
        let mut text = original.to_owned();
        let analysis = stress::analyze(&text, &[], None);
        let start = text.rfind("замок").unwrap();
        let vowel_byte = start + "зам".len();
        let character = core::byte_to_char(&text, vowel_byte);
        let mut run = |events: Vec<egui::Event>| {
            let mut hit = None;
            let mut clicked = false;
            let mut target = egui::Pos2::ZERO;
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(230.0, 400.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let mut layouter = |ui: &egui::Ui, text: &str, width: f32| {
                            let mut job = layout_poem(text, None);
                            job.wrap.max_width = width;
                            ui.fonts(|fonts| fonts.layout_job(job))
                        };
                        let output = egui::TextEdit::multiline(&mut text)
                            .id(egui::Id::new("stress_test"))
                            .desired_width(190.0)
                            .layouter(&mut layouter)
                            .show(ui);
                        let cursor = output
                            .galley
                            .pos_from_ccursor(egui::text::CCursor::new(character))
                            .translate(output.galley_pos.to_vec2());
                        target = cursor.center() + egui::vec2(3.0, 0.0);
                        hit = paint_stresses(
                            ui,
                            &output.galley,
                            output.galley_pos,
                            &output.response,
                            &analysis,
                        );
                        clicked = output.response.clicked();
                    });
                },
            );
            (target, hit, clicked, output.platform_output.cursor_icon)
        };
        let (target, _, _, _) = run(vec![]);
        let (_, hit, _, cursor) = run(vec![egui::Event::PointerMoved(target)]);
        assert_eq!(hit.map(|hit| (hit.0, hit.1)), Some((start, None)));
        assert_eq!(cursor, egui::CursorIcon::PointingHand);
        for pressed in [true, false] {
            let (_, hit, clicked, _) = run(vec![egui::Event::PointerButton {
                pos: target,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            }]);
            if !pressed {
                assert!(clicked);
                assert_eq!(hit.unwrap().1, None);
            }
        }
        drop(run);
        assert_eq!(text, original);
    }

    #[test]
    fn outline_links_have_pointer_and_native_keyboard_activation() {
        let ctx = egui::Context::default();
        let mut bounds = egui::Rect::NOTHING;
        let mut id = egui::Id::NULL;
        let mut run = |events: Vec<egui::Event>, focus: bool| {
            let mut clicked = false;
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(175.0, 240.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let response =
                            outline_link(ui, "Бридж — торжественно, почти патриотически, с инструментальным вступлением и долгой паузой", false, 17);
                        bounds = response.rect;
                        id = response.id;
                        if focus {
                            response.request_focus();
                        }
                        clicked = response.clicked();
                    });
                },
            );
            (output, clicked, bounds, id)
        };
        let (_, _, rect, _) = run(vec![], false);
        assert!(
            rect.height() > 36.0,
            "long section names must wrap without clipping: {rect:?}"
        );
        let (output, clicked, _, id) = run(vec![egui::Event::PointerMoved(rect.center())], true);
        assert!(!clicked);
        assert_eq!(
            output.platform_output.cursor_icon,
            egui::CursorIcon::PointingHand
        );
        assert!(ctx.memory(|memory| memory.has_focus(id)));
        for key in [egui::Key::Enter, egui::Key::Space] {
            let (_, clicked, _, _) = run(
                vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                false,
            );
            assert!(clicked, "section must activate from {key:?}");
            run(
                vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: false,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                false,
            );
        }
    }
}
