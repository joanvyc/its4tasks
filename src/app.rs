use std::{iter, sync::Arc, time::Duration};

use chrono::{DateTime, Datelike, Local, TimeDelta, Utc};
use eframe::egui::{
    self, Align, Button, CentralPanel, Color32, ComboBox, FontData, FontDefinitions, FontFamily,
    FontId, Frame, Id, Key, Label, Layout, Margin, Modal, Modifiers, Panel, Popup, Rect, RichText,
    ScrollArea, Sense, Shape, Stroke, StrokeKind, TextEdit, TextFormat, TextStyle, Theme,
    UiBuilder, Vec2, Visuals, text::LayoutJob,
};

use crate::log::{self, EntryKind, LogPage, Period};
use crate::model::{
    Blocker, Board, MessageKind, Note, NoteAction, SLOT_COUNT, Task, TaskState, headline,
    short_duration, shown_in_list, split_title, staleness,
};
use crate::storage;

const CARD_GAP: f32 = 12.0;
const CARD_MARGIN: i8 = 14;
const CARD_RADIUS: f32 = 10.0;
const CARD_GLOW_WIDTH: f32 = 16.0;
const COMMENT_RADIUS: f32 = 4.0;
const ROW_RADIUS: f32 = 4.0;
const ROW_GLOW_WIDTH: f32 = 6.0;
/// Fixed so the comment box doesn't shift when the message kind changes.
const KIND_PICKER_WIDTH: f32 = 96.0;
/// Longest quote of a resolved blocker shown in its Unblock message.
const QUOTE_CHARS: usize = 60;
const COMMENT_GLOW_WIDTH: f32 = 8.0;
const COMMENT_INPUT_MARGIN: Margin = Margin::symmetric(6, 4);
const LOG_TIME_WIDTH: f32 = 64.0;

/// Accent colours for each theme. The light ones are darker so they read on white.
#[derive(Clone, Copy)]
struct Palette {
    fresh: Color32,
    stale: Color32,
    blocker: Color32,
    star: Color32,
    resolved: Color32,
}

const DARK: Palette = Palette {
    fresh: Color32::from_rgb(130, 130, 130),
    stale: Color32::from_rgb(235, 195, 40),
    blocker: Color32::from_rgb(215, 85, 75),
    star: Color32::from_rgb(235, 195, 40),
    resolved: Color32::from_rgb(110, 180, 110),
};

const LIGHT: Palette = Palette {
    fresh: Color32::from_rgb(160, 160, 160),
    stale: Color32::from_rgb(215, 160, 0),
    blocker: Color32::from_rgb(200, 55, 45),
    star: Color32::from_rgb(215, 160, 0),
    resolved: Color32::from_rgb(40, 135, 60),
};

fn palette(visuals: &Visuals) -> Palette {
    if visuals.dark_mode { DARK } else { LIGHT }
}

const HEADING_SIZE: f32 = 18.0;

/// Inter Bold, for titles; falls back to the same fonts as body text.
fn bold() -> FontFamily {
    FontFamily::Name("bold".into())
}

/// Inter (the base of GitLab Sans): Medium for body text, Bold for headings.
/// egui's bundled fonts stay as fallbacks for symbols and emoji Inter lacks.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    for (name, data) in [
        (
            "Inter-Medium",
            &include_bytes!("../assets/fonts/Inter-Medium.ttf")[..],
        ),
        (
            "Inter-Bold",
            &include_bytes!("../assets/fonts/Inter-Bold.ttf")[..],
        ),
    ] {
        let data = Arc::new(FontData::from_static(data));
        fonts.font_data.insert(name.to_owned(), data);
    }
    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    let fallbacks = proportional.clone();
    proportional.insert(0, "Inter-Medium".to_owned());
    let bold_fonts = iter::once("Inter-Bold".to_owned())
        .chain(fallbacks)
        .collect();
    fonts.families.insert(bold(), bold_fonts);
    ctx.set_fonts(fonts);
    ctx.all_styles_mut(|style| {
        let heading = FontId::new(HEADING_SIZE, bold());
        style.text_styles.insert(TextStyle::Heading, heading);
    });
}

/// Gray → yellow by time since the last update.
fn freshness_color(task: &Task, now: DateTime<Utc>, visuals: &Visuals) -> Color32 {
    let colors = palette(visuals);
    let t = staleness(now - task.updated_at);
    colors.fresh.lerp_to_gamma(colors.stale, t)
}

/// Red glow along the inside edge of a blocked card or blocking comment.
fn blocked_glow_color(visuals: &Visuals) -> Color32 {
    if visuals.dark_mode {
        Color32::from_rgb(210, 60, 60).gamma_multiply(0.6)
    } else {
        Color32::from_rgb(230, 80, 80).gamma_multiply(0.45)
    }
}

/// The comment box's fill. In light mode `faint_bg_color` barely shows against
/// the card's white background.
fn comment_input_bg(visuals: &Visuals) -> Color32 {
    if visuals.dark_mode {
        visuals.faint_bg_color
    } else {
        Color32::from_gray(238)
    }
}

/// A "⋯" button opening the task's menu. Shown when `visible` or while the
/// menu is open; otherwise its space is kept empty so nothing shifts. The dots
/// are painted so they sit on the row's center line, like the focus dot.
fn task_menu(ui: &mut egui::Ui, task_id: u64, visible: bool, actions: &mut Vec<Action>) {
    let id = ui.make_persistent_id(("task_menu", task_id));
    let open = Popup::is_id_open(ui.ctx(), id.with("popup"));
    let (rect, _) = ui.allocate_exact_size(Vec2::new(20.0, 16.0), Sense::hover());
    if !visible && !open {
        return;
    }
    let response = ui.interact(rect, id, Sense::click());
    let visuals = ui.visuals();
    let active = response.hovered() || open;
    if active {
        let fill = visuals.widgets.hovered.weak_bg_fill;
        ui.painter().rect_filled(rect, 3.0, fill);
    }
    let color = if active {
        visuals.strong_text_color()
    } else {
        visuals.weak_text_color()
    };
    for dx in [-4.0, 0.0, 4.0] {
        let center = rect.center() + Vec2::new(dx, 0.0);
        ui.painter().circle_filled(center, 1.5, color);
    }
    let response = response.on_hover_text("More");
    Popup::menu(&response).show(|ui| {
        if ui.button("Delete…").clicked() {
            actions.push(Action::Delete(task_id));
        }
    });
}

/// What the delete confirmation says will go.
fn delete_prompt(task: &Task) -> String {
    let what = match task.notes.len() {
        0 => String::new(),
        1 => " and its comment".to_owned(),
        n => format!(" and its {n} comments"),
    };
    format!(
        "\"{}\"{what} will be deleted. This can't be undone.",
        task.title
    )
}

/// An on/off switch whose track turns `on_color` when on.
fn toggle_switch(
    ui: &mut egui::Ui,
    on: &mut bool,
    on_color: Color32,
    name: &str,
) -> egui::Response {
    let size = Vec2::new(28.0, 16.0);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response
        .widget_info(|| egui::WidgetInfo::selected(egui::Role::Switch, ui.is_enabled(), *on, name));
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(response.id, *on);
        let visuals = ui.style().interact_selectable(&response, *on);
        let radius = 0.5 * rect.height();
        let track = visuals.bg_fill.lerp_to_gamma(on_color, t);
        ui.painter()
            .rect(rect, radius, track, visuals.bg_stroke, StrokeKind::Inside);
        let x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), t);
        let knob = egui::pos2(x, rect.center().y);
        let knob_color = visuals.fg_stroke.color.lerp_to_gamma(Color32::WHITE, t);
        ui.painter().circle_filled(knob, radius - 2.0, knob_color);
    }
    response
}

/// `color` at the rect's edge, fading out to nothing `width` px inside it.
/// Drawn as 1px rounded outlines, each one inset a pixel further and fainter.
fn inner_glow(rect: Rect, corner_radius: f32, width: f32, color: Color32) -> Shape {
    let rings = (0..width.ceil() as usize).map(|i| {
        let inset = i as f32;
        let fade = (1.0 - inset / width).powi(2);
        Shape::rect_stroke(
            rect.shrink(inset),
            (corner_radius - inset).max(0.0),
            Stroke::new(1.0, color.gamma_multiply(fade)),
            StrokeKind::Inside,
        )
    });
    Shape::Vec(rings.collect())
}

fn state_text_color(state: TaskState, visuals: &Visuals) -> Color32 {
    match state {
        TaskState::Blocked if visuals.dark_mode => Color32::from_rgb(255, 150, 140),
        TaskState::Blocked => Color32::from_rgb(170, 40, 40),
        TaskState::Active | TaskState::Done => visuals.weak_text_color(),
    }
}

fn kind_color(kind: MessageKind, visuals: &Visuals) -> Color32 {
    match kind {
        MessageKind::Comment => visuals.weak_text_color(),
        MessageKind::Block => palette(visuals).blocker,
        MessageKind::Unblock | MessageKind::Close => palette(visuals).resolved,
    }
}

fn kind_hint(kind: MessageKind) -> &'static str {
    match kind {
        MessageKind::Comment => "Post a plain comment",
        MessageKind::Block => "Post as blocking: the task stays blocked until resolved",
        MessageKind::Unblock => "Resolve the blocker, with this as the message",
        MessageKind::Close => "Mark the task done, with this as the message",
    }
}

/// `text` cut to `max` characters, with an ellipsis if anything was cut.
fn truncate(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", text[..i].trim_end()),
        None => text.to_owned(),
    }
}

/// A comment's text, with the title of a multi-line comment in bold.
fn append_comment(job: &mut LayoutJob, text: &str, color: Color32, ui: &egui::Ui) {
    let font = TextStyle::Body.resolve(ui.style());
    let (title, body) = split_title(text);
    if let Some(title) = title {
        let bold_font = FontId::new(font.size, bold());
        job.append(title, 0.0, TextFormat::simple(bold_font, color));
        job.append("\n", 0.0, TextFormat::simple(font.clone(), color));
    }
    job.append(body, 0.0, TextFormat::simple(font, color));
}

/// An Unblock or Close message: a green marker saying what it did, then the text.
fn action_job(action: &NoteAction, note: &Note, ui: &egui::Ui) -> LayoutJob {
    let font = TextStyle::Body.resolve(ui.style());
    let format = |color| TextFormat::simple(font.clone(), color);
    let marker = match (action, note.unblocked_after()) {
        (NoteAction::Unblocked { quote, .. }, Some(after)) => format!(
            "✔ Resolved \"{}\", after {} with message: ",
            truncate(headline(quote), QUOTE_CHARS),
            short_duration(after)
        ),
        _ => "✔ Closed with message: ".to_owned(),
    };
    let mut job = LayoutJob::default();
    job.append(&marker, 0.0, format(palette(ui.visuals()).resolved));
    append_comment(&mut job, &note.text, ui.visuals().text_color(), ui);
    job
}

fn local_time(at: DateTime<Utc>) -> String {
    at.with_timezone(&Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

fn ago_text(age: TimeDelta) -> String {
    match (age.num_days(), age.num_hours(), age.num_minutes()) {
        (d, _, _) if d >= 1 => format!("{d} day{} ago", if d == 1 { "" } else { "s" }),
        (_, h, _) if h >= 1 => format!("{h} hour{} ago", if h == 1 { "" } else { "s" }),
        (_, _, m) if m >= 1 => format!("{m} min ago"),
        _ => "just now".to_owned(),
    }
}

fn updated_text(task: &Task, now: DateTime<Utc>) -> String {
    format!(
        "Updated {} · {}",
        ago_text(now - task.updated_at),
        local_time(task.updated_at)
    )
}

/// One entry of a card's comment log: timestamp/blocker line, then the text,
/// or a text box while `edit` is being written. `hovered` reveals the star,
/// edit and "mark as blocking" buttons.
#[allow(clippy::too_many_arguments)]
fn comment(
    ui: &mut egui::Ui,
    note: &Note,
    idx: usize,
    task_id: u64,
    hovered: bool,
    edit: Option<&mut NoteEdit>,
    now: DateTime<Utc>,
    actions: &mut Vec<Action>,
) {
    let colors = palette(ui.visuals());
    ui.horizontal(|ui| {
        ui.label(RichText::new(local_time(note.at)).small().weak());
        match (note.blocker, note.blocked_for(now), note.blocked_duration()) {
            (Some(Blocker::Open), Some(blocked_for), _) => {
                ui.label(
                    RichText::new("· ⛔ Blocking")
                        .small()
                        .strong()
                        .color(colors.blocker),
                );
                ui.label(
                    RichText::new(if blocked_for < TimeDelta::minutes(1) {
                        "· blocked just now".to_owned()
                    } else {
                        format!("· blocked for {}", short_duration(blocked_for))
                    })
                    .small()
                    .color(colors.blocker),
                )
                .on_hover_text(format!(
                    "Blocking since {}",
                    local_time(note.blocked_since())
                ));
            }
            (Some(Blocker::Resolved { at }), _, Some(took)) => {
                ui.label(
                    RichText::new(format!(
                        "· ✔ Resolved after {} ({})",
                        short_duration(took),
                        local_time(at)
                    ))
                    .small()
                    .weak(),
                );
            }
            _ => {}
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // Hover-only buttons are always laid out (just hidden) so the row
            // height doesn't jump on hover.
            let star = if note.starred {
                Button::new(RichText::new("★").color(colors.star))
            } else {
                Button::new(RichText::new("☆").weak())
            };
            let star = ui
                .add_visible(note.starred || hovered, star.frame(false).small())
                .on_hover_text(if note.starred { "Unstar" } else { "Star" });
            if star.clicked() {
                actions.push(Action::ToggleStar(task_id, idx));
            }
            let edit_button = ui
                .add_visible(hovered && edit.is_none(), Button::new("Edit").small())
                .on_hover_text("Edit this comment");
            if edit_button.clicked() {
                actions.push(Action::StartEdit(task_id, idx));
            }
            match note.blocker {
                Some(Blocker::Open) => {
                    if ui
                        .small_button("✔ Resolve")
                        .on_hover_text("Mark this blocker as resolved")
                        .clicked()
                    {
                        actions.push(Action::Resolve(task_id, idx));
                    }
                }
                Some(Blocker::Resolved { .. }) => {}
                None => {
                    let block = ui
                        .add_visible(hovered, Button::new("⛔ Mark blocking").small())
                        .on_hover_text("Turn this comment into a blocker");
                    if block.clicked() {
                        actions.push(Action::MarkBlocking(task_id, idx));
                    }
                }
            }
        });
    });
    if let Some(edit) = edit {
        edit_box(ui, edit, actions);
        return;
    }
    let job = match &note.action {
        Some(action) => action_job(action, note, ui),
        None => {
            let mut job = LayoutJob::default();
            append_comment(&mut job, &note.text, ui.visuals().text_color(), ui);
            job
        }
    };
    ui.add(Label::new(job).wrap());
}

/// Editing a comment: Enter saves, Shift+Enter adds a newline, Escape cancels.
fn edit_box(ui: &mut egui::Ui, edit: &mut NoteEdit, actions: &mut Vec<Action>) {
    let id = ui.make_persistent_id(("edit_note", edit.task_id, edit.idx));
    let save = ui.memory(|m| m.has_focus(id))
        && ui.input_mut(|i| !i.modifiers.shift && i.consume_key(Modifiers::NONE, Key::Enter));
    // Checked regardless of focus: egui drops the text box's focus on Escape
    // before we get to see it. Only one comment is edited at a time.
    let cancel = ui.input(|i| i.key_pressed(Key::Escape));
    if save {
        actions.push(Action::SaveEdit);
    } else if cancel {
        actions.push(Action::CancelEdit);
    }
    let response = ui.add(
        TextEdit::multiline(&mut edit.text)
            .id(id)
            .background_color(comment_input_bg(ui.visuals()))
            .margin(COMMENT_INPUT_MARGIN)
            .desired_rows(1)
            .desired_width(f32::INFINITY),
    );
    if !edit.focus_requested {
        response.request_focus();
        edit.focus_requested = true;
    }
    ui.horizontal(|ui| {
        if ui.small_button("Save").clicked() {
            actions.push(Action::SaveEdit);
        }
        if ui.small_button("Cancel").clicked() {
            actions.push(Action::CancelEdit);
        }
        ui.label(
            RichText::new("Enter to save · Shift+Enter for a new line")
                .small()
                .weak(),
        );
    });
}

fn log_heading(page: &LogPage, period: Period) -> String {
    match period {
        Period::Day => page.start.format("%Y-%m-%d · %A").to_string(),
        Period::Week => format!(
            "Week {} · {} – {}",
            page.start.iso_week().week(),
            page.start.format("%Y-%m-%d"),
            (page.start + TimeDelta::days(6)).format("%Y-%m-%d"),
        ),
    }
}

/// Marker plus text for one log entry, as a single wrapping paragraph.
fn log_entry_job(entry: &log::Entry, ui: &egui::Ui) -> LayoutJob {
    let font = TextStyle::Body.resolve(ui.style());
    let normal = ui.visuals().text_color();
    let format = |color| TextFormat::simple(font.clone(), color);
    let colors = palette(ui.visuals());
    let mut job = LayoutJob::default();
    if entry.starred {
        job.append("★ ", 0.0, format(colors.star));
    }
    let (marker, color) = match &entry.kind {
        EntryKind::Created => (Some("Created".to_owned()), ui.visuals().weak_text_color()),
        EntryKind::Comment { blocking: false } => (None, normal),
        EntryKind::Comment { blocking: true } => (Some("Blocked: ".to_owned()), colors.blocker),
        EntryKind::MarkedBlocking => (Some("Marked blocking: ".to_owned()), colors.blocker),
        EntryKind::Unblocked { after } => (
            Some(format!("Unblocked after {}: ", short_duration(*after))),
            colors.resolved,
        ),
        EntryKind::Resolved { quote, after } => (
            Some(format!(
                "✔ Resolved \"{}\", after {} with message: ",
                truncate(quote, QUOTE_CHARS),
                short_duration(*after)
            )),
            colors.resolved,
        ),
        EntryKind::Done => (Some("✔ Marked done".to_owned()), colors.resolved),
        EntryKind::Closed => (Some("✔ Closed with message: ".to_owned()), colors.resolved),
    };
    if let Some(marker) = marker {
        job.append(&marker, 0.0, format(color));
    }
    append_comment(&mut job, &entry.text, normal, ui);
    job
}

#[derive(Default)]
struct NewTaskDraft {
    title: String,
    notes: String,
    add_to_focus: bool,
    focus_requested: bool,
}

/// A comment being edited in place.
struct NoteEdit {
    task_id: u64,
    idx: usize,
    text: String,
    focus_requested: bool,
}

/// Changes requested while drawing, applied once drawing is done so we don't
/// mutate the board while iterating over it.
enum Action {
    Focus(u64),
    FocusInto(usize, u64),
    Unfocus(usize),
    /// Ask for confirmation, then delete.
    Delete(u64),
    DeleteNote(u64, usize),
    MarkBlocking(u64, usize),
    Resolve(u64, usize),
    Unblock(u64, usize),
    ToggleStar(u64, usize),
    StartEdit(u64, usize),
    SaveEdit,
    CancelEdit,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Board,
    Log,
}

pub struct TodoApp {
    board: Board,
    filter: String,
    new_task: Option<NewTaskDraft>,
    /// The task waiting for the user to confirm its deletion.
    confirm_delete: Option<u64>,
    /// Unsent comment text for each focus card.
    comment_drafts: [String; SLOT_COUNT],
    /// What posting from each card's comment box does.
    comment_kind: [MessageKind; SLOT_COUNT],
    editing: Option<NoteEdit>,
    view: View,
    log_period: Period,
    log_starred_only: bool,
    /// Leave done tasks out of the task list.
    hide_closed: bool,
    dirty: bool,
}

impl TodoApp {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        install_fonts(&cc.egui_ctx);
        cc.egui_ctx.set_theme(Theme::Light);
        Self {
            board: storage::load(),
            filter: String::new(),
            new_task: None,
            confirm_delete: None,
            comment_drafts: Default::default(),
            comment_kind: [MessageKind::Comment; SLOT_COUNT],
            editing: None,
            view: View::Board,
            log_period: Period::Day,
            log_starred_only: false,
            hide_closed: false,
            dirty: false,
        }
    }

    fn apply(&mut self, actions: Vec<Action>) {
        let now = Utc::now();
        for action in actions {
            match action {
                Action::Focus(id) => {
                    self.board.focus(id);
                }
                Action::FocusInto(slot, id) => self.board.focus_into(slot, id),
                Action::Unfocus(slot) => self.board.unfocus(slot),
                Action::Delete(id) => self.confirm_delete = Some(id),
                Action::DeleteNote(id, idx) => {
                    if let Some(task) = self.board.task_mut(id) {
                        task.delete_note(idx);
                    }
                    // The indices after it have shifted.
                    self.editing.take_if(|e| e.task_id == id);
                }
                Action::MarkBlocking(id, idx) => {
                    if let Some(task) = self.board.task_mut(id) {
                        task.set_blocking(idx, now);
                    }
                }
                Action::Resolve(id, idx) => {
                    if let Some(task) = self.board.task_mut(id) {
                        task.resolve(idx, now);
                    }
                }
                Action::Unblock(id, idx) => {
                    if let Some(task) = self.board.task_mut(id) {
                        task.unmark_blocking(idx);
                    }
                }
                Action::ToggleStar(id, idx) => {
                    if let Some(task) = self.board.task_mut(id) {
                        task.toggle_star(idx);
                    }
                }
                Action::StartEdit(id, idx) => {
                    let note = self.board.task(id).and_then(|t| t.notes.get(idx));
                    self.editing = note.map(|note| NoteEdit {
                        task_id: id,
                        idx,
                        text: note.text.clone(),
                        focus_requested: false,
                    });
                }
                Action::SaveEdit => {
                    if let Some(edit) = self.editing.take()
                        && let Some(task) = self.board.task_mut(edit.task_id)
                    {
                        task.edit_note(edit.idx, &edit.text, now);
                    }
                }
                Action::CancelEdit => self.editing = None,
            }
            self.dirty = true;
        }
    }

    fn save_if_dirty(&mut self) {
        if !self.dirty {
            return;
        }
        if let Err(e) = storage::save(&self.board) {
            eprintln!("focal: failed to save: {e}");
        }
        self.dirty = false;
    }

    fn task_list(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let plus = ui
                    .add(Button::new(RichText::new(" + ").strong()))
                    .on_hover_text("New task");
                if plus.clicked() {
                    self.new_task = Some(NewTaskDraft::default());
                }
                ui.add(
                    TextEdit::singleline(&mut self.filter)
                        .hint_text("Filter tasks…")
                        .desired_width(f32::INFINITY),
                );
            });
        });
        ui.add_space(4.0);

        let shown: Vec<u64> = self
            .board
            .tasks
            .iter()
            .filter(|t| shown_in_list(t, &self.filter, self.hide_closed))
            .map(|t| t.id)
            .collect();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!(
                    "{} of {} tasks",
                    shown.len(),
                    self.board.tasks.len()
                ))
                .small()
                .weak(),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let hint = "Leave done tasks out of this list";
                let on = &mut self.hide_closed;
                let label = Label::new(RichText::new("Hide closed").small())
                    .selectable(false)
                    .sense(Sense::click());
                if ui.add(label).on_hover_text(hint).clicked() {
                    *on = !*on;
                }
                let color = palette(ui.visuals()).resolved;
                toggle_switch(ui, on, color, "Hide closed").on_hover_text(hint);
            });
        });
        ui.separator();

        let has_free_slot = self.board.has_free_slot();
        let now = Utc::now();
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            if self.board.tasks.is_empty() {
                ui.add_space(8.0);
                ui.weak("No tasks yet. Press + to create one.");
            }
            for id in shown {
                let slot = self.board.slot_of(id);
                let Some(task) = self.board.task_mut(id) else {
                    continue;
                };
                let blocker = match task.state() {
                    TaskState::Blocked => task.open_blocker().map(|n| n.text.clone()),
                    TaskState::Active | TaskState::Done => None,
                };
                let glow = ui.painter().add(Shape::Noop);
                let row = Frame::new()
                    .corner_radius(ROW_RADIUS)
                    .inner_margin(Margin::symmetric(6, 2));
                let shown = row.show(ui, |ui| {
                    // A fixed row height, unlike `ui.horizontal`'s growing one, gives
                    // every widget (and the nested layouts) the same center line.
                    let size = Vec2::new(ui.available_width(), ui.spacing().interact_size.y + 4.0);
                    ui.allocate_ui_with_layout(size, Layout::left_to_right(Align::Center), |ui| {
                        let row_hovered = ui.rect_contains_pointer(ui.max_rect());
                        let color = if task.done {
                            ui.visuals().weak_text_color()
                        } else {
                            freshness_color(task, now, ui.visuals())
                        };
                        let (bar, bar_response) =
                            ui.allocate_exact_size(Vec2::new(4.0, 16.0), Sense::hover());
                        ui.painter().rect_filled(bar, 2.0, color);
                        let mut hover = updated_text(task, now);
                        if let Some(text) = &blocker {
                            hover = format!("Blocked: {text}\n{hover}");
                        }
                        bar_response.on_hover_text(hover);
                        let mut done = task.done;
                        if ui.checkbox(&mut done, "").changed() {
                            task.set_done(done, now);
                            self.dirty = true;
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if slot.is_some() {
                                // Painted rather than a "●" glyph, which sits on the text
                                // baseline instead of the row's center.
                                let (dot, response) =
                                    ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                                let color = ui.visuals().selection.bg_fill;
                                ui.painter().circle_filled(dot.center(), 5.0, color);
                                response.on_hover_text("In focus");
                            } else {
                                let btn = ui
                                    .add_enabled(has_free_slot, Button::new("→").small())
                                    .on_hover_text("Bring into focus")
                                    .on_disabled_hover_text("All 4 focus slots are taken");
                                if btn.clicked() {
                                    actions.push(Action::Focus(id));
                                }
                            }
                            // Always laid out, so titles don't reflow on hover.
                            task_menu(ui, id, row_hovered, actions);
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                let mut text = RichText::new(&task.title);
                                if task.done {
                                    text = text.strikethrough().weak();
                                }
                                let label = ui
                                    .dnd_drag_source(Id::unique(("task", id)), id, |ui| {
                                        ui.add(Label::new(text).truncate().selectable(false))
                                    })
                                    .response
                                    .on_hover_text("Drag onto a card to focus");
                                label.context_menu(|ui| {
                                    if ui.button("Delete…").clicked() {
                                        actions.push(Action::Delete(id));
                                    }
                                });
                            });
                        });
                    })
                });
                if blocker.is_some() {
                    let color = blocked_glow_color(ui.visuals());
                    let shape = inner_glow(shown.response.rect, ROW_RADIUS, ROW_GLOW_WIDTH, color);
                    ui.painter().set(glow, shape);
                }
            }
        });
    }

    fn cards(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let area = ui.available_rect_before_wrap();
        let cell = Vec2::new(
            ((area.width() - CARD_GAP) / 2.0).max(0.0),
            ((area.height() - CARD_GAP) / 2.0).max(0.0),
        );
        for slot in 0..SLOT_COUNT {
            let (col, row) = ((slot % 2) as f32, (slot / 2) as f32);
            let min = area.min + Vec2::new(col * (cell.x + CARD_GAP), row * (cell.y + CARD_GAP));
            let rect = Rect::from_min_size(min, cell);
            ui.scope_builder(
                UiBuilder::new().max_rect(rect).id_salt(("card", slot)),
                |ui| self.card(ui, slot, cell, actions),
            );
        }
        ui.allocate_rect(area, Sense::hover());
    }

    fn card(&mut self, ui: &mut egui::Ui, slot: usize, cell: Vec2, actions: &mut Vec<Action>) {
        let visuals = ui.visuals().clone();
        let now = Utc::now();
        let task_id = self.board.slots[slot];
        let task = task_id.and_then(|id| self.board.task(id));
        let stroke = match task {
            Some(task) if !task.done => Stroke::new(2.0, freshness_color(task, now, &visuals)),
            _ => Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
        };
        let blocked = task.is_some_and(|t| t.state() == TaskState::Blocked);
        let has_task = task.is_some();
        // The background (and glow) is painted into this slot once the card's rect
        // is known, so it stays behind the card's contents.
        let background = ui.painter().add(Shape::Noop);
        let frame = Frame::new()
            .inner_margin(Margin::same(CARD_MARGIN))
            .corner_radius(CARD_RADIUS)
            .stroke(stroke);
        let content = cell - Vec2::splat(2.0 * (f32::from(CARD_MARGIN) + stroke.width));

        let (zone, dropped) = ui.dnd_drop_zone::<u64, _>(frame, |ui| {
            ui.set_min_size(content);
            ui.set_max_size(content);
            let Some(task) = task_id.and_then(|id| self.board.task_mut(id)) else {
                ui.centered_and_justified(|ui| {
                    ui.weak("Drop a task here or press → in the list");
                });
                return;
            };
            ui.horizontal(|ui| {
                let mut done = task.done;
                if ui.checkbox(&mut done, "").changed() {
                    task.set_done(done, now);
                    self.dirty = true;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add(Button::new("✕").frame(false))
                        .on_hover_text("Send back to the list")
                        .clicked()
                    {
                        actions.push(Action::Unfocus(slot));
                    }
                    task_menu(ui, task.id, true, actions);
                    let title = TextEdit::singleline(&mut task.title)
                        .font(TextStyle::Heading)
                        .frame(Frame::NONE)
                        .desired_width(f32::INFINITY);
                    if ui.add(title).changed() {
                        task.touch(now);
                        self.dirty = true;
                    }
                });
            });
            ui.horizontal(|ui| {
                let state = task.state();
                ui.label(
                    RichText::new(state.label())
                        .small()
                        .strong()
                        .color(state_text_color(state, &visuals)),
                );
                ui.label(RichText::new("·").small().weak());
                let (dot, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                ui.painter()
                    .circle_filled(dot.center(), 4.0, freshness_color(task, now, &visuals));
                ui.label(
                    RichText::new(ago_text(now - task.updated_at))
                        .small()
                        .weak(),
                )
                .on_hover_text(updated_text(task, now));
            });
            ui.separator();

            // Lay out bottom-up so the comment box stays pinned to the card's
            // bottom edge and the comment log takes whatever height is left.
            ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                // Enter posts, Shift+Enter adds a newline.
                let input_id = ui.make_persistent_id(("comment_input", slot));
                let draft = &mut self.comment_drafts[slot];
                let kinds = task.message_kinds();
                let kind = &mut self.comment_kind[slot];
                if !kinds.contains(kind) {
                    *kind = MessageKind::Comment;
                }
                let submit = ui.memory(|m| m.has_focus(input_id))
                    && ui.input_mut(|i| {
                        !i.modifiers.shift && i.consume_key(Modifiers::NONE, Key::Enter)
                    });
                // In a bottom-up layout a row only reserves its initial height and
                // then grows downward past the card's edge, so reserve the comment
                // box's height up front: 2 to 6 lines, as measured last frame.
                let line = ui.text_style_height(&TextStyle::Body);
                let min_height = 2.0 * line + COMMENT_INPUT_MARGIN.sum().y;
                let max_height = 6.0 * line + COMMENT_INPUT_MARGIN.sum().y;
                let height_id = input_id.with("height");
                let row_height = ui
                    .data(|d| d.get_temp::<f32>(height_id))
                    .unwrap_or(min_height);
                let row = Vec2::new(ui.available_width(), row_height);
                ui.allocate_ui_with_layout(row, Layout::left_to_right(Align::Min), |ui| {
                    // Lined up with the comment box's first line of text.
                    let picker_row =
                        Vec2::new(KIND_PICKER_WIDTH, line + COMMENT_INPUT_MARGIN.sum().y);
                    ui.allocate_ui_with_layout(
                        picker_row,
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            let selected =
                                RichText::new(kind.label()).color(kind_color(*kind, &visuals));
                            ComboBox::from_id_salt(("comment_kind", slot))
                                .selected_text(selected)
                                .width(KIND_PICKER_WIDTH - ui.spacing().item_spacing.x)
                                .show_ui(ui, |ui| {
                                    for &k in &kinds {
                                        // Weak text is hard to read on the
                                        // selection highlight.
                                        let mut text = RichText::new(k.label());
                                        if k != MessageKind::Comment {
                                            text = text.color(kind_color(k, &visuals));
                                        }
                                        ui.selectable_value(kind, k, text)
                                            .on_hover_text(kind_hint(k));
                                    }
                                })
                                .response
                                .on_hover_text(kind_hint(*kind));
                        },
                    );
                    let hint = match *kind {
                        MessageKind::Comment => "Add a comment… (Enter to post)",
                        MessageKind::Block => "Describe what you're waiting for… (Enter to post)",
                        MessageKind::Unblock => "What unblocked it?… (Enter to post)",
                        MessageKind::Close => "Closing note… (Enter to post)",
                    };
                    let edit = ScrollArea::vertical()
                        .id_salt(("comment_input_scroll", slot))
                        .max_height(max_height)
                        .show(ui, |ui| {
                            ui.add(
                                TextEdit::multiline(draft)
                                    .id(input_id)
                                    .hint_text(hint)
                                    .background_color(comment_input_bg(&visuals))
                                    .margin(COMMENT_INPUT_MARGIN)
                                    .desired_rows(2)
                                    .desired_width(f32::INFINITY),
                            )
                        })
                        .inner;
                    let height = edit.rect.height().clamp(min_height, max_height);
                    if (height - row_height).abs() > 0.5 {
                        ui.data_mut(|d| d.insert_temp(height_id, height));
                        ui.ctx().request_discard("comment box resized");
                    }
                });
                if submit && !draft.trim().is_empty() {
                    task.post(draft, *kind, now);
                    draft.clear();
                    *kind = MessageKind::Comment;
                    self.dirty = true;
                }
                ui.add_space(4.0);

                ScrollArea::vertical()
                    .id_salt(("comments", slot))
                    .auto_shrink(false)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        ui.with_layout(Layout::top_down(Align::Min), |ui| {
                            if task.notes.is_empty() {
                                ui.weak("No comments yet.");
                            }
                            for (idx, note) in task.notes.iter().enumerate() {
                                let id = task.id;
                                let edit = self
                                    .editing
                                    .as_mut()
                                    .filter(|e| e.task_id == id && e.idx == idx);
                                let frame = match note.blocker {
                                    // Highlighted with a glow painted below.
                                    Some(Blocker::Open) => Frame::new(),
                                    Some(Blocker::Resolved { .. }) => {
                                        Frame::new().stroke(Stroke::new(
                                            1.0,
                                            visuals.widgets.noninteractive.bg_stroke.color,
                                        ))
                                    }
                                    None => Frame::new(),
                                }
                                .corner_radius(COMMENT_RADIUS)
                                .inner_margin(Margin::symmetric(6, 4));
                                // The click sense sits below the comment's buttons, so they
                                // still get their clicks.
                                let entry = ui
                                    .scope_builder(UiBuilder::new().sense(Sense::click()), |ui| {
                                        let hovered = ui.response().contains_pointer();
                                        let glow = ui.painter().add(Shape::Noop);
                                        let shown = frame.show(ui, |ui| {
                                            ui.set_width(ui.available_width());
                                            comment(ui, note, idx, id, hovered, edit, now, actions);
                                        });
                                        if note.is_open_blocker() {
                                            ui.painter().set(
                                                glow,
                                                inner_glow(
                                                    shown.response.rect,
                                                    COMMENT_RADIUS,
                                                    COMMENT_GLOW_WIDTH,
                                                    blocked_glow_color(&visuals),
                                                ),
                                            );
                                        }
                                    })
                                    .response;
                                entry.context_menu(|ui| {
                                    match note.blocker {
                                        Some(Blocker::Open) => {
                                            if ui.button("Resolve").clicked() {
                                                actions.push(Action::Resolve(id, idx));
                                            }
                                            if ui.button("Not blocking").clicked() {
                                                actions.push(Action::Unblock(id, idx));
                                            }
                                        }
                                        Some(Blocker::Resolved { .. }) => {
                                            if ui.button("Reopen blocker").clicked() {
                                                actions.push(Action::MarkBlocking(id, idx));
                                            }
                                            if ui.button("Not blocking").clicked() {
                                                actions.push(Action::Unblock(id, idx));
                                            }
                                        }
                                        None => {
                                            if ui.button("Mark as blocking").clicked() {
                                                actions.push(Action::MarkBlocking(id, idx));
                                            }
                                        }
                                    }
                                    if ui.button("Edit").clicked() {
                                        actions.push(Action::StartEdit(id, idx));
                                    }
                                    let star = if note.starred { "Unstar" } else { "Star" };
                                    if ui.button(star).clicked() {
                                        actions.push(Action::ToggleStar(id, idx));
                                    }
                                    if ui.button("Delete").clicked() {
                                        actions.push(Action::DeleteNote(id, idx));
                                    }
                                });
                                ui.add_space(4.0);
                            }
                        });
                    });
            });
        });
        if has_task {
            let rect = zone.response.rect;
            let mut shapes = vec![Shape::rect_filled(
                rect,
                CARD_RADIUS,
                visuals.extreme_bg_color,
            )];
            if blocked {
                let inside = rect.shrink(stroke.width);
                shapes.push(inner_glow(
                    inside,
                    CARD_RADIUS - stroke.width,
                    CARD_GLOW_WIDTH,
                    blocked_glow_color(&visuals),
                ));
            }
            ui.painter().set(background, Shape::Vec(shapes));
        }
        if let Some(id) = dropped {
            actions.push(Action::FocusInto(slot, *id));
        }
    }

    fn view_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.view, View::Board, "Board");
            ui.selectable_value(&mut self.view, View::Log, "Log");
            if self.view == View::Log {
                ui.separator();
                ComboBox::from_id_salt("log_period")
                    .selected_text(self.log_period.label())
                    .show_ui(ui, |ui| {
                        for period in [Period::Day, Period::Week] {
                            ui.selectable_value(&mut self.log_period, period, period.label());
                        }
                    });
                ui.separator();
                let on = &mut self.log_starred_only;
                toggle_switch(ui, on, palette(ui.visuals()).star, "Starred only");
                let label = Label::new("Starred only")
                    .selectable(false)
                    .sense(Sense::click());
                if ui.add(label).clicked() {
                    *on = !*on;
                }
            }
            // Text rather than egui's ☀/🌙 buttons: drawing those emoji makes
            // other symbols such as ✔ switch to the colour emoji font.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let mut theme = ui.ctx().theme();
                let before = theme;
                ui.selectable_value(&mut theme, Theme::Dark, "Dark");
                ui.selectable_value(&mut theme, Theme::Light, "Light");
                if theme != before {
                    ui.ctx().set_theme(theme);
                }
            });
        });
        ui.add_space(6.0);
    }

    fn log_view(&mut self, ui: &mut egui::Ui) {
        let pages = log::build(&self.board, self.log_period, self.log_starred_only, &Local);
        let time_format = match self.log_period {
            Period::Day => "%H:%M",
            Period::Week => "%a %H:%M",
        };
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            if pages.is_empty() {
                ui.add_space(8.0);
                ui.weak(if self.log_starred_only {
                    "No starred comments yet. Star a comment from its card."
                } else {
                    "Nothing logged yet."
                });
            }
            for page in &pages {
                ui.add_space(8.0);
                ui.label(RichText::new(log_heading(page, self.log_period)).heading());
                ui.separator();
                for group in &page.groups {
                    ui.add_space(6.0);
                    ui.indent(("log_task", page.start, group.task_id), |ui| {
                        ui.label(RichText::new(&group.title).strong().family(bold()));
                        ui.add_space(2.0);
                        for entry in &group.entries {
                            ui.horizontal_top(|ui| {
                                let time = entry.at.with_timezone(&Local).format(time_format);
                                let (rect, _) = ui.allocate_exact_size(
                                    Vec2::new(
                                        LOG_TIME_WIDTH,
                                        ui.text_style_height(&TextStyle::Body),
                                    ),
                                    Sense::hover(),
                                );
                                ui.put(
                                    rect,
                                    Label::new(RichText::new(time.to_string()).small().weak()),
                                );
                                ui.add(Label::new(log_entry_job(entry, ui)).wrap());
                            });
                        }
                    });
                }
            }
            ui.add_space(8.0);
        });
    }

    fn delete_modal(&mut self, ctx: &egui::Context) {
        let Some(task) = self.confirm_delete.and_then(|id| self.board.task(id)) else {
            self.confirm_delete = None;
            return;
        };
        let id = task.id;
        let prompt = delete_prompt(task);
        let mut confirm = false;
        let mut cancel = false;
        let modal = Modal::new(Id::unique("delete_task_modal")).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.heading("Delete task?");
            ui.add_space(8.0);
            ui.add(Label::new(prompt).wrap());
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let text = RichText::new("Delete").color(Color32::WHITE);
                let delete = Button::new(text).fill(palette(ui.visuals()).blocker);
                if ui.add(delete).clicked() {
                    confirm = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
        if confirm {
            self.board.delete_task(id);
            self.editing.take_if(|e| e.task_id == id);
            self.confirm_delete = None;
            self.dirty = true;
        } else if cancel || modal.should_close() {
            self.confirm_delete = None;
        }
    }

    fn new_task_modal(&mut self, ctx: &egui::Context) {
        let has_free_slot = self.board.has_free_slot();
        let Some(draft) = &mut self.new_task else {
            return;
        };
        let mut submit = false;
        let mut cancel = false;
        let modal = Modal::new(Id::unique("new_task_modal")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading("New task");
            ui.add_space(8.0);
            let title = ui.add(
                TextEdit::singleline(&mut draft.title)
                    .hint_text("Title")
                    .desired_width(f32::INFINITY),
            );
            if !draft.focus_requested {
                title.request_focus();
                draft.focus_requested = true;
            }
            if title.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                submit = true;
            }
            ui.add_space(4.0);
            ui.add(
                TextEdit::multiline(&mut draft.notes)
                    .hint_text("First comment (optional)")
                    .desired_rows(5)
                    .desired_width(f32::INFINITY),
            );
            ui.add_space(4.0);
            ui.add_enabled(
                has_free_slot,
                egui::Checkbox::new(&mut draft.add_to_focus, "Add to focus"),
            )
            .on_disabled_hover_text("All 4 focus slots are taken");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let can_create = !draft.title.trim().is_empty();
                if ui.add_enabled(can_create, Button::new("Create")).clicked() {
                    submit = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });

        if submit && !draft.title.trim().is_empty() {
            let draft = self.new_task.take().unwrap_or_default();
            let id = self
                .board
                .add_task(draft.title.trim().to_owned(), &draft.notes, Utc::now());
            if draft.add_to_focus {
                self.board.focus(id);
            }
            self.dirty = true;
        } else if cancel || modal.should_close() {
            self.new_task = None;
        }
    }
}

impl eframe::App for TodoApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let mut actions = Vec::new();
        Panel::right("task_list")
            .resizable(true)
            .default_size(320.0)
            .min_size(240.0)
            .show(ui, |ui| self.task_list(ui, &mut actions));
        CentralPanel::default().show(ui, |ui| {
            self.view_bar(ui);
            match self.view {
                View::Board => self.cards(ui, &mut actions),
                View::Log => self.log_view(ui),
            }
        });
        self.new_task_modal(&ui.ctx().clone());
        self.delete_modal(&ui.ctx().clone());
        self.apply(actions);
        self.save_if_dirty();
        // Keep the freshness colors drifting while the app sits idle.
        ui.ctx().request_repaint_after(Duration::from_secs(60));
    }

    fn on_exit(&mut self) {
        self.save_if_dirty();
    }
}
