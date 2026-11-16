use std::{
    collections::BTreeSet,
    ops::Range,
    path::PathBuf,
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};

use eframe::egui::{self, Color32, FontId, RichText, Stroke, TextFormat};

use crate::{
    core, formatter, history, history::Snapshot as DocumentState, lexicon::Lexicon, storage,
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
    Dictionary(u64, Result<(Arc<Lexicon>, Vec<String>), String>),
    Search(u64, Vec<core::Candidate>),
    UpdateCheck(Result<Option<crate::updater::Manifest>, String>),
    UpdateInstalled(Result<(), String>),
}

enum PendingAction {
    New,
    Open(PathBuf),
    Close,
    CopyRevision(DocumentState),
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
    text: String,
    saved_text: String,
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
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_style(&cc.egui_ctx);
        let smoke_test = std::env::args().any(|arg| arg == "--smoke-test");
        let recovery = if smoke_test {
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
        });
        let history_temp =
            smoke_test.then(|| tempfile::tempdir().expect("smoke-test history directory"));
        let history_path = history_temp
            .as_ref()
            .map(|dir| dir.path().to_owned())
            .unwrap_or_else(|| storage::data_directory().join("history"));
        let repaint = cc.egui_ctx.clone();
        let (history, document_id, file_path, history_error) =
            match history::History::open(history_path, move || repaint.request_repaint()) {
                Ok((history, previous, warning)) => {
                    let (document, path) = if let Some((summary, snapshot)) = previous {
                        session.text = snapshot.text;
                        session.document_title = snapshot.title;
                        session.rhyme_links = snapshot.rhyme_links;
                        recovered = true;
                        (summary.document, summary.path)
                    } else {
                        (history::new_document(), None)
                    };
                    (Some(history), document, path, warning)
                }
                Err(error) => (None, history::new_document(), None, Some(error)),
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
            saved_text: if recovered {
                String::new()
            } else {
                session.text.clone()
            },
            saved_title: if recovered {
                None
            } else {
                session.document_title.clone()
            },
            document_title: session.document_title,
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
                "Восстановлен последний текст. Сохраните его в файл.".into()
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
            update_busy: false,
            update_available: None,
            update_message: String::new(),
            show_updates: false,
            updates_button_rect: None,
            smoke_test,
            smoke_stage: 0,
            smoke_started: Instant::now(),
        };
        app.rhyme_links.retain(|link| link.is_valid(&app.text));
        app.record_revision("Начало сессии");
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
            if self.update_busy { ui.spinner(); }
            if let Some(manifest) = self.update_available.clone() {
                if ui.add_enabled(!self.update_busy, egui::Button::new("Скачать и установить")).clicked() {
                    self.update_busy = true;
                    self.update_message = "Загружаем и проверяем подпись…".into();
                    let tx = self.tx.clone(); let ctx = ctx.clone();
                    std::thread::spawn(move || { let _ = tx.send(Background::UpdateInstalled(crate::updater::install(&manifest))); ctx.request_repaint(); });
                }
                ui.label("Перед установкой проверяются подпись и контрольная сумма. Текст останется открыт; новая версия запустится при следующем открытии приложения.");
            }
            if ui.add_enabled(!self.update_busy, egui::Button::new("Проверить обновления")).clicked() { self.check_updates(ctx); }
            ui.hyperlink_to("Страница выпусков", format!("{}/releases", crate::updater::REPOSITORY));
        });
        self.show_updates = open;
    }

    fn set_status(&mut self, message: impl Into<String>, error: bool) {
        self.status = message.into();
        self.status_is_error = error;
    }

    fn is_dirty(&self) -> bool {
        self.text != self.saved_text || self.document_title != self.saved_title
    }

    fn load_dictionary(&mut self, ctx: &egui::Context) {
        self.dictionary_ticket += 1;
        self.dictionary_loading = true;
        self.dictionary_error = None;
        self.lexicon = None;
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
        let anchor = self.anchor.trim().to_owned();
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
                Background::Dictionary(ticket, result) if ticket == self.dictionary_ticket => {
                    self.dictionary_loading = false;
                    match result {
                        Ok((lexicon, parts)) => {
                            self.lexicon = Some(lexicon);
                            self.parts_of_speech = parts;
                            self.schedule_search();
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
                            "Обновление установлено. Закройте и снова откройте Poetrhymer.".into()
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
        }
    }

    fn restore_snapshot(&mut self, state: DocumentState) {
        self.text = state.text;
        self.document_title = state.title;
        self.rhyme_links = state.rhyme_links;
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
        match storage::write_text(&path, &self.text) {
            Ok(()) => {
                self.last_document_folder = path.parent().map(std::path::Path::to_owned);
                self.file_path = Some(path);
                self.saved_text = self.text.clone();
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
            PendingAction::Close => {
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            PendingAction::New => self.replace_document(String::new(), None),
            PendingAction::CopyRevision(snapshot) => {
                self.replace_document(snapshot.text.clone(), None);
                self.restore_snapshot(snapshot);
                self.saved_text.clear();
                self.saved_title = None;
                self.after_document_change("Копия версии");
                self.set_status(
                    "Версия открыта как новый текст. Исходная история сохранена.",
                    false,
                );
            }
            PendingAction::Open(path) => match storage::read_text(&path) {
                Ok(text) => self.replace_document(text, Some(path)),
                Err(error) => self.set_status(error, true),
            },
        }
    }

    fn replace_document(&mut self, text: String, path: Option<PathBuf>) {
        self.document_id = history::new_document();
        self.rhyme_links.clear();
        self.document_title = None;
        self.saved_title = None;
        self.text = text;
        self.saved_text = self.text.clone();
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
                    .filter_map(|line| line.section.as_ref().map(|title| (title, line.span.start)))
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
                for (title, start) in sections {
                    if ui
                        .add_sized(
                            [ui.available_width(), 34.0],
                            egui::Button::new(title).frame(false),
                        )
                        .clicked()
                    {
                        self.navigate_to(start);
                    }
                }
                ui.add_space(22.0);
                ui.separator();
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!self.undo.is_empty(), egui::Button::new("Отменить"))
                        .on_hover_text("Ctrl+Z")
                        .clicked()
                    {
                        self.undo();
                    }
                    if ui
                        .add_enabled(!self.redo.is_empty(), egui::Button::new("Повторить"))
                        .on_hover_text("Ctrl+Y")
                        .clicked()
                    {
                        self.redo();
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
            ui.label(RichText::new(format!("{} · {} слогов", core::line_label(&self.text, selection.line), line_syllables)).color(MUTED).size(12.0));
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
            let vowels: Vec<char> = unaccented.chars().filter(|c| "аеёиоуыэюяАЕЁИОУЫЭЮЯ".contains(*c)).collect();
            let mut stress_choice = None;
            if !vowels.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Ударение:").small().color(MUTED));
                    for (index, vowel) in vowels.iter().enumerate() {
                        if ui.small_button(format!("{} · {}", index + 1, vowel)).on_hover_text("Указать ударный слог вручную").clicked() {
                            stress_choice = Some(index);
                        }
                    }
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
                filter_changed |= ui.checkbox(&mut self.match_syllables, "Слогов").changed();
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
            let list_height = (ui.available_height() - 130.0).max(70.0);
            egui::ScrollArea::vertical().id_salt("rhyme_results").max_height(list_height).show(ui, |ui| {
                for (index, candidate) in self.candidates.iter().enumerate() {
                    if self.closer_only && candidate.score < 120 { continue; }
                    let response = ui.add_sized([ui.available_width(), 32.0], egui::Button::new(
                        RichText::new(&candidate.stressed).size(17.0)
                    ).selected(self.chosen == Some(index)));
                    response.clone().on_hover_text(format!("{} слогов · {}", candidate.syllables, candidate.part_of_speech));
                    if response.clicked() { self.chosen = Some(index); }
                }
            });
            if let Some(candidate) = self.chosen.and_then(|index| self.candidates.get(index)) {
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(8.0);
                ui.label(RichText::new("ПРЕДПРОСМОТР").size(10.0).color(MUTED));
                if let Some(selection) = &self.selection {
                    if let Ok(preview) = core::replace_word(&self.text, selection.span.clone(), &candidate.word) {
                        if let Some(line) = core::parse_lines(&preview).iter().find(|line| line.index == selection.line) {
                            ui.label(RichText::new(preview[line.span.clone()].trim()).size(17.0).color(ACCENT));
                        }
                    }
                }
                ui.add_space(8.0);
                if ui.add_sized([ui.available_width(), 36.0], egui::Button::new("Заменить слово").fill(Color32::from_rgb(85, 66, 40))).clicked() {
                    self.apply_candidate();
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
                ui.horizontal(|ui| {
                    ui.label(RichText::new("ВАШ ТЕКСТ").size(11.0).strong().color(MUTED));
                    if ui.small_button("Форматировать").on_hover_text("Убрать Markdown, вынести # название в верхнюю панель и оформить секции в [скобках]").clicked() {
                        self.format_document();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new("Последние слова — точки для новых рифм")
                                .size(12.0)
                                .color(MUTED),
                        );
                    });
                });
                ui.add_space(24.0);
                let editor_id = ui.make_persistent_id("poem_text");
                if let Some(cursor) = self.requested_cursor.take() {
                    if let Some(mut state) = egui::TextEdit::load_state(ctx, editor_id) {
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
                let mut layouter = |ui: &egui::Ui, text: &str, wrap_width: f32| {
                    let mut job = layout_poem(text, selected.as_ref());
                    job.wrap.max_width = wrap_width;
                    ui.fonts(|fonts| fonts.layout_job(job))
                };
                let mut clicked_position = None;
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
                        let numbers = core::verse_numbers(&self.text);
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
                    self.select_word(core::char_to_byte(&self.text, character));
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

    // Developer-only end-to-end command test. It uses a separate fixture,
    // never restores or writes the user's recovery file, and captures the GPU frame.
    fn smoke_update(&mut self, ctx: &egui::Context) {
        let directory = storage::app_directory().join("tmp/smoke-test");
        let finish = |ctx: &egui::Context, result: Result<(), String>| {
            let _ = std::fs::create_dir_all(&directory);
            let report = match result {
                Ok(()) => {
                    "PASS: import, search, replace, undo, redo, preview and screenshot".into()
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
                self.saved_text = self.text.clone();
                self.select_word(self.text.find("фуража").unwrap());
                self.smoke_stage = 1;
            } else {
                finish(ctx, Err("багажа missing from candidates".into()));
            }
        } else if self.smoke_stage == 1 && !self.search_loading && self.search_due.is_none() {
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
            if std::env::args().any(|arg| arg == "--smoke-updates") {
                self.show_updates = true;
                self.update_message = "Установлена актуальная версия".into();
            }
            self.set_status("Проверено: словарь, поиск, замена, отмена и повтор.", false);
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
            let session = storage::Session {
                text: self.text.clone(),
                document_title: self.document_title.clone(),
                dictionary_folder: self.dictionary_folder.clone(),
                rhyme_links: self.rhyme_links.clone(),
                last_document_folder: self.last_document_folder.clone(),
            };
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
        } else if self.smoke_stage == 2 && self.smoke_started.elapsed() > Duration::from_millis(400)
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
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.receive_background();
        self.receive_history();
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
            if let Err(error) = storage::save_recovery(&storage::Session {
                text: self.text.clone(),
                document_title: self.document_title.clone(),
                dictionary_folder: self.dictionary_folder.clone(),
                rhyme_links: self.rhyme_links.clone(),
                last_document_folder: self.last_document_folder.clone(),
            }) {
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
        self.dictionary_window(ctx);
        self.updates_window(ctx);
        self.history_window(ctx);
        self.pending_window(ctx);
        if self.smoke_test {
            self.smoke_update(ctx);
        }
        if self.search_due.is_some()
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
        if self.smoke_test {
            return;
        }
        let _ = storage::save_recovery(&storage::Session {
            text: self.text.clone(),
            document_title: self.document_title.clone(),
            dictionary_folder: self.dictionary_folder.clone(),
            rhyme_links: self.rhyme_links.clone(),
            last_document_folder: self.last_document_folder.clone(),
        });
    }
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
