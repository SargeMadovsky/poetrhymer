use std::{
    collections::BTreeSet,
    ops::Range,
    path::PathBuf,
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};

use eframe::egui::{self, Color32, FontId, RichText, Stroke, TextFormat};

use crate::{core, lexicon::Lexicon, storage};

const SAMPLE: &str = "[Куплет 1]\n«Где ты был, Семён Петрович?»\nТот ответил не спеша:\n«Поле было. Я работал.\nЗаготовил фуража».\n";
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
}

pub struct PoetApp {
    text: String,
    saved_text: String,
    file_path: Option<PathBuf>,
    revision: u64,
    undo: Vec<String>,
    redo: Vec<String>,
    last_typing: Option<Instant>,
    selection: Option<Selection>,
    anchor: String,
    anchor_line: Option<usize>,
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
    update_busy: bool,
    update_available: Option<crate::updater::Manifest>,
    update_message: String,
    show_updates: bool,
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
        let recovered = recovery.is_some();
        let default_folder = storage::app_directory().join("Rhymes 3");
        let session = recovery.unwrap_or_else(|| storage::Session {
            text: SAMPLE.to_owned(),
            dictionary_folder: default_folder.to_string_lossy().into_owned(),
        });
        let (tx, rx) = mpsc::channel();
        let mut app = Self {
            saved_text: if recovered {
                String::new()
            } else {
                session.text.clone()
            },
            text: session.text,
            file_path: None,
            revision: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            last_typing: None,
            selection: None,
            anchor: String::new(),
            anchor_line: None,
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
            update_busy: false,
            update_available: None,
            update_message: String::new(),
            show_updates: false,
            smoke_test,
            smoke_stage: 0,
            smoke_started: Instant::now(),
        };
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
        egui::Window::new("Обновления Poetrhymer").open(&mut open).default_width(440.0).show(ctx, |ui| {
            ui.label(format!("Текущая версия: {}", env!("CARGO_PKG_VERSION")));
            ui.label(&self.update_message);
            if self.update_busy { ui.spinner(); }
            if ui.add_enabled(!self.update_busy, egui::Button::new("Проверить обновления")).clicked() { self.check_updates(ctx); }
            if let Some(manifest) = self.update_available.clone() {
                ui.label("Перед установкой проверяются подпись и контрольная сумма. Текст останется открыт; новая версия запустится при следующем открытии приложения.");
                if ui.add_enabled(!self.update_busy, egui::Button::new("Скачать и установить")).clicked() {
                    self.update_busy = true;
                    self.update_message = "Загружаем и проверяем подпись…".into();
                    let tx = self.tx.clone(); let ctx = ctx.clone();
                    std::thread::spawn(move || { let _ = tx.send(Background::UpdateInstalled(crate::updater::install(&manifest))); ctx.request_repaint(); });
                }
            }
            ui.hyperlink_to("Страница выпусков", format!("{}/releases", crate::updater::REPOSITORY));
        });
        self.show_updates = open;
    }

    fn set_status(&mut self, message: impl Into<String>, error: bool) {
        self.status = message.into();
        self.status_is_error = error;
    }

    fn is_dirty(&self) -> bool {
        self.text != self.saved_text
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
                    .map(|entry| entry.part_of_speech.clone())
                    .filter(|part| !part.is_empty())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
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
            self.search_due = Some(Instant::now() + Duration::from_millis(200));
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
        // A visible, editable starting choice: the previous-but-one verse line,
        // or the immediately previous line when only one is available.
        let preceding: Vec<_> = lines
            .iter()
            .filter(|other| {
                other.index < line.index && other.last_word.is_some() && other.section.is_none()
            })
            .collect();
        let preferred = preceding.iter().rev().nth(1).or_else(|| preceding.last());
        if let Some(other) = preferred {
            self.anchor = self.text[other.last_word.clone().unwrap()].to_owned();
            self.anchor_line = Some(other.index);
        } else {
            self.anchor.clear();
            self.anchor_line = None;
        }
        self.schedule_search();
    }

    fn after_document_change(&mut self) {
        self.revision += 1;
        self.selection = None;
        self.anchor_line = None;
        self.invalidate_search();
        self.autosave_due = Some(Instant::now() + Duration::from_millis(800));
    }

    fn push_undo(&mut self, previous: String) {
        self.undo.push(previous);
        if self.undo.len() > 150 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn undo(&mut self) {
        if let Some(previous) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.text, previous));
            self.last_typing = None;
            self.after_document_change();
            self.set_status("Изменение отменено", false);
        }
    }

    fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.text, next));
            self.last_typing = None;
            self.after_document_change();
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
        let old_word = selection.word.clone();
        let new_word = candidate.word.clone();
        let anchor = self.anchor.clone();
        let anchor_line = self.anchor_line;
        match core::replace_word(&self.text, selection.span.clone(), &new_word) {
            Ok(text) => {
                let previous = std::mem::replace(&mut self.text, text);
                self.push_undo(previous);
                self.last_typing = None;
                self.after_document_change();
                self.requested_cursor = Some(core::byte_to_char(&self.text, target_start));
                self.select_word(target_start);
                self.anchor = anchor;
                self.anchor_line = anchor_line;
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
        let path = if save_as || self.file_path.is_none() {
            rfd::FileDialog::new()
                .add_filter("Текст UTF-8", &["txt"])
                .set_file_name(
                    self.file_path
                        .as_ref()
                        .and_then(|path| path.file_name())
                        .and_then(|name| name.to_str())
                        .unwrap_or("Мой текст.txt"),
                )
                .save_file()
        } else {
            self.file_path.clone()
        };
        let Some(path) = path else { return false };
        match storage::write_text(&path, &self.text) {
            Ok(()) => {
                self.file_path = Some(path);
                self.saved_text = self.text.clone();
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
            PendingAction::Open(path) => match storage::read_text(&path) {
                Ok(text) => self.replace_document(text, Some(path)),
                Err(error) => self.set_status(error, true),
            },
        }
    }

    fn replace_document(&mut self, text: String, path: Option<PathBuf>) {
        self.text = text;
        self.saved_text = self.text.clone();
        self.file_path = path;
        self.undo.clear();
        self.redo.clear();
        self.last_typing = None;
        self.after_document_change();
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
                        .file_path
                        .as_ref()
                        .and_then(|path| path.file_name())
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "Новый текст".into());
                    ui.label(
                        RichText::new(format!("{name}{}", if self.is_dirty() { " •" } else { "" }))
                            .color(MUTED),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(12.0);
                        if ui.button("Обновления").clicked() {
                            self.show_updates = true;
                        }
                        if ui.button("Словарь").clicked() {
                            self.show_dictionary = !self.show_dictionary;
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
                        RichText::new("Добавьте [Куплет] или [Припев] отдельной строкой.")
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
                        self.requested_cursor = Some(core::byte_to_char(&self.text, start));
                    }
                }
                ui.add_space(22.0);
                ui.separator();
                ui.add_space(14.0);
                ui.label(RichText::new("Текст остаётся вашим").size(13.0).color(INK));
                ui.add_space(5.0);
                ui.label(
                    RichText::new("Рифмы ищутся локально. Вы выбираете каждую замену.")
                        .size(12.0)
                        .color(MUTED),
                );
                ui.add_space(20.0);
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
            ui.label(RichText::new(format!("Строка {} · {} слогов", selection.line + 1, line_syllables)).color(MUTED).size(12.0));
            let selected_line = selection.line;
            ui.add_space(15.0);
            ui.label("Рифма к слову");
            if ui.add(egui::TextEdit::singleline(&mut self.anchor).desired_width(f32::INFINITY).hint_text("Например, спеша́")).changed() {
                self.anchor_line = None;
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
                self.schedule_search();
            }
            let mut anchor_choice = None;
            egui::ComboBox::from_id_salt("anchor_line")
                .width(ui.available_width() - 8.0)
                .selected_text(self.anchor_line.map(|line| format!("Из строки {}", line + 1)).unwrap_or_else(|| "Выбрать строку-опору".into()))
                .show_ui(ui, |ui| {
                    for line in core::parse_lines(&self.text) {
                        if line.index == selected_line { continue }
                        if let Some(span) = line.last_word {
                            let word = self.text[span].to_owned();
                            if ui.selectable_label(self.anchor_line == Some(line.index), format!("{} · {}", line.index + 1, word)).clicked() {
                                anchor_choice = Some((line.index, word));
                            }
                        }
                    }
                });
            if let Some((line, word)) = anchor_choice {
                self.anchor_line = Some(line);
                self.anchor = word;
                self.schedule_search();
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
                ui.horizontal(|ui| { ui.spinner(); ui.label("Читаем словарь Rhymes…"); });
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
                let previous = self.text.clone();
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
                egui::ScrollArea::vertical()
                    .id_salt("poem_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let output = egui::TextEdit::multiline(&mut self.text)
                            .id(editor_id)
                            .font(FontId::proportional(21.0))
                            .desired_width(f32::INFINITY)
                            .desired_rows(22)
                            .frame(false)
                            .hint_text("[Куплет 1]\nНачните писать или вставьте заготовку…")
                            .layouter(&mut layouter)
                            .show(ui);
                        changed = output.response.changed();
                        if output.response.clicked() && !output.response.double_clicked() {
                            if let Some(pos) = output.response.interact_pointer_pos() {
                                let cursor = output.galley.cursor_from_pos(pos - output.galley_pos);
                                clicked_position = Some(cursor.ccursor.index);
                            }
                        }
                    });
                if changed {
                    let now = Instant::now();
                    let coalesce = self
                        .last_typing
                        .is_some_and(|last| now.duration_since(last) < Duration::from_millis(650));
                    if !coalesce {
                        self.push_undo(previous);
                    } else {
                        self.redo.clear();
                    }
                    self.last_typing = Some(now);
                    self.after_document_change();
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
                ui.horizontal(|ui| { ui.spinner(); ui.label("Читаем словарь…"); });
            } else if let Some(error) = &self.dictionary_error {
                ui.colored_label(Color32::LIGHT_RED, error);
            } else if let Some(lexicon) = &self.lexicon {
                ui.colored_label(Color32::from_rgb(127, 201, 173), format!("Подключено: {} записей", format_count(lexicon.entries.len())));
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
            self.chosen = self.candidates.iter().position(|c| c.word == "багажа");
            self.set_status("Проверено: словарь, поиск, замена, отмена и повтор.", false);
            self.smoke_stage = 2;
        } else if self.smoke_stage == 2 {
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
        if ctx.input(|input| input.viewport().close_requested())
            && !self.allow_close
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
                dictionary_folder: self.dictionary_folder.clone(),
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
                        if self.dictionary_loading {
                            ui.spinner();
                        } else if let Some(lexicon) = &self.lexicon {
                            ui.label(
                                RichText::new(format!(
                                    "Rhymes · частичный импорт · {} записей",
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
        self.pending_window(ctx);
        if self.smoke_test {
            self.smoke_update(ctx);
        }
        if self.search_due.is_some()
            || self.autosave_due.is_some()
            || self.dictionary_loading
            || self.search_loading
        {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.smoke_test {
            return;
        }
        let _ = storage::save_recovery(&storage::Session {
            text: self.text.clone(),
            dictionary_folder: self.dictionary_folder.clone(),
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
