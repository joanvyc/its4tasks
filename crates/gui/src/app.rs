use std::{collections::HashSet, iter, sync::Arc, sync::mpsc::Receiver, time::Duration};

use chrono::{DateTime, Datelike, Local, NaiveDate, TimeDelta, Utc};
use eframe::egui::{
    self, Align, Atom, Button, CentralPanel, Color32, ComboBox, CursorIcon, DragAndDrop, FontData,
    FontDefinitions, FontFamily, FontId, Frame, Id, Key, Label, Layout, Margin, Modal, Modifiers,
    Panel, Popup, Rect, RichText, ScrollArea, Sense, Shape, Stroke, StrokeKind, TextBuffer,
    TextEdit, TextFormat, TextStyle, Theme, UiBuilder, Vec2, Visuals,
    text::{CCursor, LayoutJob},
};

use crate::api::{self, Change};
use crate::client::Client;
use crate::files::{self, Picked, Saved};
use crate::import;
use crate::log::{self, EntryKind, Filter, LogPage, Period};
use crate::model::{
    Blocker, Board, Command, CommandError, MessageKind, MoveError, NextStep, Note, NoteAction,
    SLOT_COUNT, Submit, Task, TaskState, headline, list_guides, short_duration, split_title,
    staleness,
};
use crate::unsent::{self, NewTask, Unsent};
use crate::welcome;
use uuid::Uuid;
use web_time::Instant;

const CARD_GAP: f32 = 12.0;
const CARD_MARGIN: i8 = 14;
const CARD_RADIUS: f32 = 10.0;
const CARD_GLOW_WIDTH: f32 = 16.0;
const COMMENT_RADIUS: f32 = 4.0;
const ROW_RADIUS: f32 = 4.0;
const ROW_GLOW_WIDTH: f32 = 6.0;
/// Fixed so the comment box doesn't shift when the message kind changes.
const KIND_PICKER_WIDTH: f32 = 104.0;
/// Longest quote of a resolved blocker shown in its Unblock message.
const QUOTE_CHARS: usize = 60;
const COMMENT_GLOW_WIDTH: f32 = 8.0;
const COMMENT_INPUT_MARGIN: Margin = Margin::symmetric(6, 4);
const LOG_TIME_WIDTH: f32 = 64.0;
/// How far each level of subtasks is indented in the task list.
const SUBTASK_INDENT: f32 = 16.0;
/// How long a card's edge flashes when navigating to it, in seconds.
const FLASH_SECS: f64 = 0.8;
/// The edge drawn around a flashing or highlighted card.
const HIGHLIGHT_WIDTH: f32 = 3.0;

/// Accent colours for each theme. The light ones are darker so they read on white.
#[derive(Clone, Copy)]
struct Palette {
    fresh: Color32,
    stale: Color32,
    blocker: Color32,
    star: Color32,
    resolved: Color32,
    next: Color32,
    /// A valid `/command` in the comment box.
    command: Color32,
}

const DARK: Palette = Palette {
    fresh: Color32::from_rgb(130, 130, 130),
    stale: Color32::from_rgb(235, 195, 40),
    blocker: Color32::from_rgb(215, 85, 75),
    star: Color32::from_rgb(235, 195, 40),
    resolved: Color32::from_rgb(110, 180, 110),
    next: Color32::from_rgb(110, 165, 245),
    command: Color32::from_rgb(110, 165, 245),
};

const LIGHT: Palette = Palette {
    fresh: Color32::from_rgb(160, 160, 160),
    stale: Color32::from_rgb(215, 160, 0),
    blocker: Color32::from_rgb(200, 55, 45),
    star: Color32::from_rgb(215, 160, 0),
    resolved: Color32::from_rgb(40, 135, 60),
    next: Color32::from_rgb(30, 100, 210),
    command: Color32::from_rgb(30, 100, 210),
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

/// Blue glow along the inside edge of a card or comment with a next step.
fn next_glow_color(visuals: &Visuals) -> Color32 {
    if visuals.dark_mode {
        Color32::from_rgb(70, 130, 240).gamma_multiply(0.6)
    } else {
        Color32::from_rgb(60, 120, 230).gamma_multiply(0.45)
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

/// The task's menu entries, shared by the "⋯" button and the list's right-click menu.
/// Small weak text that works as a link: strong and underlined while hovered.
/// `salt` keeps its id, and so its hover state, the same between frames.
fn link_label(ui: &mut egui::Ui, salt: &str, text: &str) -> egui::Response {
    ui.push_id(salt, |ui| {
        // The label takes the next id; its hover is known from last frame, so
        // the text is styled before it's laid out and nothing reflows.
        let hovered = ui
            .ctx()
            .read_response(ui.next_auto_id())
            .is_some_and(|r| r.hovered());
        let mut text = RichText::new(text).small();
        text = if hovered {
            text.color(ui.visuals().strong_text_color()).underline()
        } else {
            text.weak()
        };
        ui.add(
            Label::new(text)
                .truncate()
                .selectable(false)
                .sense(Sense::click()),
        )
        .on_hover_cursor(CursorIcon::PointingHand)
    })
    .inner
}

fn task_menu_items(ui: &mut egui::Ui, task_id: Uuid, is_subtask: bool, actions: &mut Vec<Action>) {
    if ui.button("Add subtask…").clicked() {
        actions.push(Action::AddSubtask(task_id, String::new()));
    }
    if is_subtask && ui.button("Make root task").clicked() {
        actions.push(
            Change::SetParent {
                task: task_id,
                parent: None,
            }
            .into(),
        );
    }
    if ui.button("Delete…").clicked() {
        actions.push(Action::Delete(task_id));
    }
}

/// The task's "⋯" menu. See `dots_menu`.
fn task_menu(
    ui: &mut egui::Ui,
    task_id: Uuid,
    is_subtask: bool,
    visible: bool,
    actions: &mut Vec<Action>,
) {
    let id = ui.make_persistent_id(("task_menu", task_id));
    dots_menu(ui, id, visible, |ui| {
        task_menu_items(ui, task_id, is_subtask, actions);
    });
}

/// A "⋯" button opening a menu. Shown when `visible` or while the menu is
/// open; otherwise its space is kept empty so nothing shifts. The dots are
/// painted so they sit on the row's center line, like the focus dot.
fn dots_menu(ui: &mut egui::Ui, id: Id, visible: bool, add_contents: impl FnOnce(&mut egui::Ui)) {
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
    Popup::menu(&response).show(add_contents);
}

/// A comment's menu entries, shared by its "⋯" button and its right-click menu.
fn comment_menu_items(ui: &mut egui::Ui, note: &Note, can_block: bool, actions: &mut Vec<Action>) {
    match note.next_step {
        Some(NextStep::Open { .. }) => {
            if ui
                .button("✔ Done")
                .on_hover_text("Tick this step off")
                .clicked()
            {
                actions.push(Change::ToggleStep { note: note.id }.into());
            }
            if ui
                .button("Promote to subtask…")
                .on_hover_text("Turn this step into a subtask of its own")
                .clicked()
            {
                actions.push(Action::StartPromote(note.id));
            }
            if ui.button("Not a next step").clicked() {
                actions.push(Change::UnmarkStep { note: note.id }.into());
            }
        }
        Some(NextStep::Done { .. }) => {
            if ui.button("Reopen step").clicked() {
                actions.push(Change::ToggleStep { note: note.id }.into());
            }
            if ui.button("Not a next step").clicked() {
                actions.push(Change::UnmarkStep { note: note.id }.into());
            }
        }
        Some(NextStep::Promoted { .. }) => {}
        None => blocker_menu_items(ui, note, can_block, actions),
    }
    if ui.button("Edit").clicked() {
        actions.push(Action::StartEdit(note.id));
    }
    let star = if note.starred { "Unstar" } else { "Star" };
    if ui.button(star).clicked() {
        actions.push(Change::ToggleStar { note: note.id }.into());
    }
    let unmovable = match (&note.action, note.next_step) {
        (Some(NoteAction::Closed), _) => Some(MoveError::CloseNote),
        (_, Some(NextStep::Promoted { .. })) => Some(MoveError::Promoted),
        _ => None,
    };
    if ui
        .add_enabled(unmovable.is_none(), Button::new("Move to task…"))
        .on_hover_text("Move it to another task, e.g. if it was posted on the wrong one")
        .on_disabled_hover_text(unmovable.map(|e| e.to_string()).unwrap_or_default())
        .clicked()
    {
        actions.push(Action::StartMove(note.id));
    }
    if ui.button("Delete").clicked() {
        actions.push(Change::DeleteNote { note: note.id }.into());
    }
}

/// The blocker entries of a comment's menu, for comments that aren't next steps.
fn blocker_menu_items(ui: &mut egui::Ui, note: &Note, can_block: bool, actions: &mut Vec<Action>) {
    match note.blocker {
        Some(Blocker::Open) => {
            if ui
                .button("✔ Resolve")
                .on_hover_text("Mark this blocker as resolved")
                .clicked()
            {
                actions.push(Change::Resolve { note: note.id }.into());
            }
            if ui.button("Not blocking").clicked() {
                actions.push(Change::Unblock { note: note.id }.into());
            }
        }
        Some(Blocker::Resolved { .. }) => {
            let reopen = Button::new("Reopen blocker");
            if ui
                .add_enabled(can_block, reopen)
                .on_disabled_hover_text(CommandError::AlreadyBlocked.to_string())
                .clicked()
            {
                actions.push(Change::MarkBlocking { note: note.id }.into());
            }
            if ui.button("Not blocking").clicked() {
                actions.push(Change::Unblock { note: note.id }.into());
            }
        }
        None => {
            let mark = Button::new("⛔ Mark as blocking");
            if ui
                .add_enabled(can_block, mark)
                .on_hover_text("Turn this comment into a blocker")
                .on_disabled_hover_text(CommandError::AlreadyBlocked.to_string())
                .clicked()
            {
                actions.push(Change::MarkBlocking { note: note.id }.into());
            }
            if note.action.is_none()
                && ui
                    .button("→ Mark as next step")
                    .on_hover_text("The next thing to do here, and it's up to you")
                    .clicked()
            {
                actions.push(Change::MarkStep { note: note.id }.into());
            }
        }
    }
}

/// A small triangle pointing right when folded, down when not. Painted so it
/// sits on the row's center line, unlike a ▸/▾ glyph.
fn fold_arrow(center: egui::Pos2, folded: bool, color: Color32) -> Shape {
    let points = if folded {
        [
            Vec2::new(-2.5, -4.0),
            Vec2::new(3.5, 0.0),
            Vec2::new(-2.5, 4.0),
        ]
    } else {
        [
            Vec2::new(-4.0, -2.5),
            Vec2::new(4.0, -2.5),
            Vec2::new(0.0, 3.5),
        ]
    };
    Shape::convex_polygon(points.map(|p| center + p).to_vec(), color, Stroke::NONE)
}

/// What the delete confirmation says will go.
fn delete_prompt(task: &Task, subtasks: usize) -> String {
    let plural = |n: usize, what: &str| match n {
        1 => format!("its {what}"),
        n => format!("its {n} {what}s"),
    };
    let what: Vec<String> = [(task.notes.len(), "comment"), (subtasks, "subtask")]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, what)| plural(n, what))
        .collect();
    let what = match what.as_slice() {
        [] => String::new(),
        what => format!(" and {}", what.join(" and ")),
    };
    format!(
        "\"{}\"{what} will be deleted. This can't be undone.",
        task.title
    )
}

/// Tree guide lines for a task list row at `depth`, in the indent before its
/// freshness bar (see `list_guides`). Each level's line runs under its parent's
/// bar, from the gap above the row so it meets the row before.
fn tree_guides(ui: &egui::Ui, depth: usize, guides: &[bool], has_shown_child: bool) {
    let rect = ui.max_rect();
    // The row's frame adds 2 px around this rect, then the gap to the next row.
    let top = rect.top() - 2.0 - ui.spacing().item_spacing.y;
    let bottom = rect.bottom() + 2.0;
    let mid = rect.center().y;
    let x = |level: usize| rect.left() + (level as f32 - 1.0) * SUBTASK_INDENT + 2.0;
    let stroke = Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color);
    let painter = ui.painter();
    for (level, runs_on) in (1..=depth).zip(guides) {
        if level < depth {
            if *runs_on {
                painter.vline(x(level), top..=bottom, stroke);
            }
        } else {
            let end = if *runs_on { bottom } else { mid };
            painter.vline(x(level), top..=end, stroke);
            let bar = rect.left() + depth as f32 * SUBTASK_INDENT;
            painter.hline(x(level)..=bar - 2.0, mid, stroke);
        }
    }
    if has_shown_child {
        // From below this row's bar (16 px tall, centered) down to its children.
        painter.vline(x(depth + 1), mid + 8.0..=bottom, stroke);
    }
}

/// Why a blocked task is blocked and for how long, for hover texts.
fn blocked_text(board: &Board, task: &Task, now: DateTime<Utc>) -> Option<String> {
    let blocked_for = board.blocked_for(task.id, now)?;
    let mut reasons: Vec<String> = task
        .open_blocker()
        .map(|n| n.text.clone())
        .into_iter()
        .collect();
    if board.blocked_by_subtasks(task.id) {
        reasons.push("all subtasks are blocked".to_owned());
    }
    Some(format!(
        "Blocked for {}: {}",
        short_duration(blocked_for),
        reasons.join("\nand ")
    ))
}

/// A task's open next steps, for hover texts.
fn next_steps_text(task: &Task) -> String {
    let steps: Vec<&str> = task.open_steps().map(|n| headline(&n.text)).collect();
    format!("Next: {}", steps.join("\nand "))
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
        TaskState::Ready => palette(visuals).next,
        TaskState::Active | TaskState::Done => visuals.weak_text_color(),
    }
}

fn kind_color(kind: MessageKind, visuals: &Visuals) -> Color32 {
    match kind {
        MessageKind::Comment => visuals.weak_text_color(),
        MessageKind::Star => palette(visuals).star,
        MessageKind::Next => palette(visuals).next,
        MessageKind::Block => palette(visuals).blocker,
        MessageKind::Unblock | MessageKind::Close => palette(visuals).resolved,
    }
}

fn kind_hint(kind: MessageKind) -> &'static str {
    match kind {
        MessageKind::Comment => "Post a plain comment",
        MessageKind::Star => "Post a starred comment: it shows in the starred log",
        MessageKind::Next => {
            "Post the next step: it's up to you, so the task shows blue until ticked off"
        }
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

/// One line for a comment in the move dialog: what kind it is and its headline.
fn moved_note_job(note: &Note, ui: &egui::Ui) -> LayoutJob {
    let font = TextStyle::Body.resolve(ui.style());
    let format = |color| TextFormat::simple(font.clone(), color);
    let colors = palette(ui.visuals());
    let mut job = LayoutJob::default();
    if note.starred {
        job.append("★ ", 0.0, format(colors.star));
    }
    let marker = match (note.blocker, &note.action) {
        _ if note.is_open_step() => Some(("→ Next step: ", colors.next)),
        _ if note.next_step.is_some() => Some(("✔ Done step: ", colors.resolved)),
        (Some(Blocker::Open), _) => Some(("⛔ Blocking: ", colors.blocker)),
        (Some(Blocker::Resolved { .. }), _) => Some(("Resolved blocker: ", colors.blocker)),
        (_, Some(NoteAction::Unblocked { .. })) => {
            Some(("✔ Resolved it with message: ", colors.resolved))
        }
        _ => None,
    };
    if let Some((marker, color)) = marker {
        job.append(marker, 0.0, format(color));
    }
    job.append(headline(&note.text), 0.0, format(ui.visuals().text_color()));
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
/// or a text box while `edit` is being written. `hovered` reveals the star
/// and the "⋯" menu, whose "mark as blocking" is enabled only when `can_block`.
/// `promoted_to` is the title of the subtask a promoted step became.
#[allow(clippy::too_many_arguments)]
fn comment(
    ui: &mut egui::Ui,
    note: &Note,
    hovered: bool,
    can_block: bool,
    promoted_to: Option<&str>,
    edit: Option<&mut NoteEdit>,
    now: DateTime<Utc>,
    actions: &mut Vec<Action>,
) {
    let colors = palette(ui.visuals());
    ui.horizontal(|ui| {
        if let Some(step @ (NextStep::Open { .. } | NextStep::Done { .. })) = note.next_step {
            let mut done = matches!(step, NextStep::Done { .. });
            let hint = if done {
                "Reopen this step"
            } else {
                "Tick this step off"
            };
            // No spacing after the box: its label is empty.
            ui.spacing_mut().icon_spacing = 0.0;
            if ui.checkbox(&mut done, "").on_hover_text(hint).changed() {
                actions.push(Change::ToggleStep { note: note.id }.into());
            }
        }
        ui.label(RichText::new(local_time(note.at)).small().weak());
        match note.next_step {
            Some(NextStep::Open { since }) => {
                ui.label(
                    RichText::new("· → Next step")
                        .small()
                        .strong()
                        .color(colors.next),
                )
                .on_hover_text(format!("Next step since {}", local_time(since)));
            }
            Some(NextStep::Done { at, .. }) => {
                ui.label(
                    RichText::new(format!("· ✔ Done ({})", local_time(at)))
                        .small()
                        .weak(),
                );
            }
            Some(NextStep::Promoted { at, .. }) => {
                ui.label(
                    RichText::new(format!("· → Promoted ({})", local_time(at)))
                        .small()
                        .weak(),
                );
            }
            None => {}
        }
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
                actions.push(Change::ToggleStar { note: note.id }.into());
            }
            let id = ui.make_persistent_id(("comment_menu", note.id));
            dots_menu(ui, id, hovered && edit.is_none(), |ui| {
                comment_menu_items(ui, note, can_block, actions);
            });
        });
    });
    if let Some(edit) = edit {
        edit_box(ui, edit, actions);
        return;
    }
    if let Some(NextStep::Promoted { subtask, .. }) = note.next_step {
        let title = promoted_to.unwrap_or(headline(&note.text));
        let mut job = LayoutJob::default();
        let font = TextStyle::Body.resolve(ui.style());
        let marker = TextFormat::simple(font.clone(), colors.next);
        job.append("→ Promoted to subtask: ", 0.0, marker);
        let mut link = TextFormat::simple(font, ui.visuals().text_color());
        link.underline = Stroke::new(1.0, ui.visuals().weak_text_color());
        job.append(title, 0.0, link);
        let label = Label::new(job).wrap().sense(Sense::click());
        if promoted_to.is_some() {
            let response = ui.add(label).on_hover_text("Focus the subtask");
            if response.clicked() {
                actions.push(Change::Focus { task: subtask }.into());
            }
        } else {
            ui.add(label).on_hover_text("The subtask was deleted");
        }
        return;
    }
    let job = match &note.action {
        Some(action) => action_job(action, note, ui),
        None => {
            let mut job = LayoutJob::default();
            let done = matches!(note.next_step, Some(NextStep::Done { .. }));
            let color = if done {
                ui.visuals().weak_text_color()
            } else {
                ui.visuals().text_color()
            };
            append_comment(&mut job, &note.text, color, ui);
            if done {
                for section in &mut job.sections {
                    section.format.strikethrough = Stroke::new(1.0, color);
                }
            }
            job
        }
    };
    ui.add(Label::new(job).wrap());
}

/// Editing a comment: Enter saves, Shift+Enter adds a newline, Escape cancels.
fn edit_box(ui: &mut egui::Ui, edit: &mut NoteEdit, actions: &mut Vec<Action>) {
    let id = ui.make_persistent_id(("edit_note", edit.note));
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
        EntryKind::Comment { blocking: true, .. } => (Some("Blocked: ".to_owned()), colors.blocker),
        EntryKind::Comment {
            next_step: true, ..
        } => (Some("Next step: ".to_owned()), colors.next),
        EntryKind::Comment { .. } => (None, normal),
        EntryKind::MarkedBlocking => (Some("Marked blocking: ".to_owned()), colors.blocker),
        EntryKind::MarkedNextStep => (Some("Marked next step: ".to_owned()), colors.next),
        EntryKind::StepDone => (Some("✔ Did: ".to_owned()), colors.resolved),
        EntryKind::Promoted { title } => {
            (Some(format!("→ Promoted to subtask: {title}")), colors.next)
        }
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

/// A task's title and entries in the log, with its subtasks as indented
/// subsections below.
fn log_group(
    ui: &mut egui::Ui,
    group: &log::TaskGroup,
    page: NaiveDate,
    is_subtask: bool,
    time_format: &str,
) {
    ui.indent(("log_task", page, group.task_id), |ui| {
        let title = RichText::new(&group.title).strong().family(bold());
        if is_subtask {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(RichText::new("↳").weak());
                ui.label(title);
            });
        } else {
            ui.label(title);
        }
        ui.add_space(2.0);
        for entry in &group.entries {
            ui.horizontal_top(|ui| {
                let time = entry.at.with_timezone(&Local).format(time_format);
                let (rect, _) = ui.allocate_exact_size(
                    Vec2::new(LOG_TIME_WIDTH, ui.text_style_height(&TextStyle::Body)),
                    Sense::hover(),
                );
                ui.put(
                    rect,
                    Label::new(RichText::new(time.to_string()).small().weak()),
                );
                ui.add(Label::new(log_entry_job(entry, ui)).wrap());
            });
        }
        for sub in &group.subgroups {
            ui.add_space(4.0);
            log_group(ui, sub, page, true, time_format);
        }
    });
}

#[derive(Default)]
struct NewTaskDraft {
    /// Creating a subtask of this task.
    parent: Option<Uuid>,
    /// Promoting this comment of `parent`, a next step, to the subtask.
    from_step: Option<Uuid>,
    title: String,
    notes: String,
    add_to_focus: bool,
    focus_requested: bool,
}

/// A comment being edited in place.
struct NoteEdit {
    note: Uuid,
    text: String,
    focus_requested: bool,
}

/// A comment waiting for the user to pick the task it moves to.
struct NoteMove {
    note: Uuid,
    target: Option<Uuid>,
}

/// What was requested while drawing, done once drawing is done so we don't
/// change the board while iterating over it.
enum Action {
    /// A change to the board, sent to the API.
    Change(Change),
    /// Ask for confirmation, then delete.
    Delete(Uuid),
    /// Open the new task dialog for a subtask of this task, with this title.
    AddSubtask(Uuid, String),
    /// Open the new subtask dialog to turn this next step into a subtask.
    StartPromote(Uuid),
    StartEdit(Uuid),
    /// Open the dialog for moving a comment to another task.
    StartMove(Uuid),
    /// Go to a task from the card in slot `from`: flash its card if it's in
    /// focus (`at` is when, in egui time), or else put it in place of `from`.
    Navigate {
        from: usize,
        task: Uuid,
        at: f64,
    },
    SaveEdit,
    CancelEdit,
}

impl From<Change> for Action {
    fn from(change: Change) -> Self {
        Action::Change(change)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Board,
    Log,
}

/// What the app asks of whatever runs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// Go back to choosing where the board is.
    SwitchBoard,
}

pub struct TodoApp {
    /// The board is only read through `api.board()` and changed through
    /// `api.apply`, which go to the server.
    api: Client,
    /// Whether whatever runs the app can switch to another board, so the
    /// menu offers it.
    switchable: bool,
    request: Option<Request>,
    /// An `its4tasks.json` being picked for importing.
    picking: Option<Receiver<Picked>>,
    /// An export being saved.
    saving: Option<Receiver<Saved>>,
    /// Why the last import or export failed.
    file_problem: Option<String>,
    filter: String,
    new_task: Option<NewTaskDraft>,
    /// The task waiting for the user to confirm its deletion.
    confirm_delete: Option<Uuid>,
    /// Unsent comment text for each focus card.
    comment_drafts: [String; SLOT_COUNT],
    /// What posting from each card's comment box does.
    comment_kind: [MessageKind; SLOT_COUNT],
    /// Why the last command typed into each card's comment box failed, for
    /// the task it was typed for.
    comment_error: [Option<(Uuid, String)>; SLOT_COUNT],
    editing: Option<NoteEdit>,
    moving: Option<NoteMove>,
    view: View,
    log_period: Period,
    log_filter: Filter,
    /// Tasks left out of the log, picked in the task list while the log is
    /// shown. Hidden rather than shown ones, so new tasks are shown.
    log_hidden: HashSet<Uuid>,
    /// Leave done tasks out of the task list.
    hide_closed: bool,
    /// Whether work kept from before signing in again (`unsent`) has been
    /// put back.
    restored: bool,
    /// When unsent work was last kept.
    kept: Instant,
    /// When each focus slot was last used, from `use_tick`, for picking the
    /// least recently used one. 0 for not yet this session.
    slot_used: [u64; SLOT_COUNT],
    use_tick: u64,
    /// The card flashing after navigating to it, and since when (egui time).
    flash: Option<(usize, f64)>,
    /// The card the open "Replace highlighted task" menu would replace.
    replacing: Option<usize>,
}

/// Sets up fonts and theme. Once, before showing anything.
pub fn setup(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.set_theme(Theme::Light);
}

impl TodoApp {
    /// The app on the board `Client::connect` finds: in the browser, the
    /// page's server's.
    pub fn new(cc: &eframe::CreationContext) -> Self {
        setup(&cc.egui_ctx);
        let ctx = cc.egui_ctx.clone();
        Self::with_client(Client::connect(move || ctx.request_repaint()), false)
    }

    /// The app on `api`'s board. `switchable` offers switching to another
    /// board, see `take_request`.
    pub fn with_client(api: Client, switchable: bool) -> Self {
        Self {
            api,
            switchable,
            request: None,
            picking: None,
            saving: None,
            file_problem: None,
            filter: String::new(),
            new_task: None,
            confirm_delete: None,
            comment_drafts: Default::default(),
            comment_kind: [MessageKind::Comment; SLOT_COUNT],
            comment_error: Default::default(),
            editing: None,
            moving: None,
            view: View::Board,
            log_period: Period::Day,
            log_filter: Filter::All,
            log_hidden: HashSet::new(),
            hide_closed: false,
            restored: false,
            kept: Instant::now(),
            slot_used: [0; SLOT_COUNT],
            use_tick: 0,
            flash: None,
            replacing: None,
        }
    }

    pub fn client(&self) -> &Client {
        &self.api
    }

    pub fn client_mut(&mut self) -> &mut Client {
        &mut self.api
    }

    /// What isn't on the server yet: refused changes, and drafts.
    fn unsent(&self) -> Unsent {
        Unsent {
            changes: self.api.held(),
            comment_drafts: unsent::drafts_by_task(&self.api.board().slots, &self.comment_drafts),
            editing: self.editing.as_ref().map(|e| (e.note, e.text.clone())),
            new_task: self.new_task.as_ref().map(|d| NewTask {
                parent: d.parent,
                from_step: d.from_step,
                title: d.title.clone(),
                notes: d.notes.clone(),
                add_to_focus: d.add_to_focus,
            }),
        }
    }

    /// Keeps unsent work in the browser, for after signing in again.
    fn keep_unsent(&mut self) {
        if let Some(user) = self.api.user() {
            unsent::save(user, &self.unsent());
        }
        self.kept = Instant::now();
    }

    /// Puts back work kept before signing in again, once the board is in.
    fn restore_unsent(&mut self) {
        let Some(user) = self.api.user() else {
            return;
        };
        self.restored = true;
        let Some(kept) = unsent::take(user) else {
            return;
        };
        for change in kept.changes {
            // Some may no longer apply, say to a task deleted elsewhere.
            let _ = self.api.apply(change);
        }
        let board = self.api.board();
        unsent::restore_drafts(&board.slots, &mut self.comment_drafts, &kept.comment_drafts);
        if let Some((note, text)) = kept.editing
            && board.find_note(note).is_some()
        {
            self.editing = Some(NoteEdit {
                note,
                text,
                focus_requested: false,
            });
        }
        if let Some(draft) = kept.new_task {
            self.new_task = Some(NewTaskDraft {
                parent: draft.parent,
                from_step: draft.from_step,
                title: draft.title,
                notes: draft.notes,
                add_to_focus: draft.add_to_focus,
                focus_requested: false,
            });
        }
    }

    /// What the app asked for since last asked, if anything.
    pub fn take_request(&mut self) -> Option<Request> {
        self.request.take()
    }

    /// Asks for an `its4tasks.json` to add to the board.
    fn pick_import(&mut self, ctx: &egui::Context) {
        if self.picking.is_none() {
            let ctx = ctx.clone();
            self.picking = Some(files::pick_board(move || ctx.request_repaint()));
        }
    }

    /// Saves the board as shown to an `its4tasks.json`.
    fn export(&mut self, ctx: &egui::Context) {
        if self.saving.is_none() {
            let json = import::export(self.api.board(), Utc::now());
            let ctx = ctx.clone();
            self.saving = Some(files::save_export(json, move || ctx.request_repaint()));
        }
    }

    /// Notes how saving an export went, once it has.
    fn poll_export(&mut self) {
        let Some(saving) = &self.saving else {
            return;
        };
        let Ok(saved) = saving.try_recv() else {
            return;
        };
        self.saving = None;
        if let Some(Err(e)) = saved {
            self.file_problem = Some(format!("Couldn't export: {e}"));
        }
    }

    /// Imports the `its4tasks.json` picked, once it is.
    fn poll_import(&mut self) {
        let Some(picking) = &self.picking else {
            return;
        };
        let Ok(picked) = picking.try_recv() else {
            return;
        };
        self.picking = None;
        self.file_problem = match picked {
            None => None,
            Some(Err(e)) => Some(format!("Couldn't import: {e}")),
            Some(Ok(board)) => self
                .api
                .apply(Change::Import {
                    board: import::fresh_ids(board),
                })
                .err()
                .map(|e| format!("Couldn't import: {e}")),
        };
    }

    /// Until the server first answers: rather than an empty board.
    fn loading(&mut self, ui: &mut egui::Ui) {
        CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() / 2.0 - 40.0).max(16.0));
                ui.spinner();
                ui.add_space(8.0);
                ui.weak("Loading your board…");
                let problem = match self.api.error() {
                    Some(e) => Some(format!("Can't reach the server: {e}")),
                    None if self.api.signed_out() => Some("Signed out.".to_owned()),
                    None => None,
                };
                if let Some(problem) = problem {
                    ui.add_space(12.0);
                    ui.colored_label(palette(ui.visuals()).blocker, problem);
                }
                #[cfg(target_arch = "wasm32")]
                if self.api.signed_out() && ui.button("Sign in again").clicked() {
                    self.keep_unsent();
                    crate::client::sign_in_again();
                }
            });
        });
        // Keep fetching until it answers.
        ui.ctx().request_repaint_after(Duration::from_secs(1));
    }

    /// For a new user: start a board, or bring one.
    fn landing(&mut self, ui: &mut egui::Ui) {
        CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() / 2.0 - 110.0).max(16.0));
                ui.heading(RichText::new("Welcome to its4tasks").strong());
                ui.add_space(8.0);
                ui.label(
                    "Your board is empty. Start with a short tour, or bring your tasks along.",
                );
                ui.add_space(20.0);
                let size = Vec2::new(240.0, 34.0);
                let start =
                    ui.add(Button::new(RichText::new("Start a new board").strong()).min_size(size));
                if start.clicked() {
                    self.change(Change::Import {
                        board: import::fresh_ids(welcome::board(api::now())),
                    });
                }
                ui.add_space(8.0);
                let import = ui
                    .add_enabled(
                        self.picking.is_none(),
                        Button::new("Import an its4tasks.json…").min_size(size),
                    )
                    .on_hover_text(
                        "Add the tasks of an its4tasks.json, or a board.json from an older its4tasks",
                    );
                if import.clicked() {
                    self.pick_import(ui.ctx());
                }
                if self.switchable {
                    ui.add_space(8.0);
                    if ui
                        .add(Button::new("Use another board…").frame(false))
                        .clicked()
                    {
                        self.request = Some(Request::SwitchBoard);
                    }
                }
                if let Some(problem) = self.file_problem.as_deref().or(self.api.error()) {
                    ui.add_space(16.0);
                    ui.colored_label(palette(ui.visuals()).blocker, problem);
                }
            });
        });
    }

    fn apply(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Change(change) => {
                    let focused = match change {
                        Change::Focus { task } | Change::FocusInto { task, .. } => Some(task),
                        _ => None,
                    };
                    self.change(change);
                    if let Some(slot) = focused.and_then(|t| self.api.board().slot_of(t)) {
                        self.mark_used(slot);
                    }
                }
                Action::Navigate { from, task, at } => match self.api.board().slot_of(task) {
                    Some(slot) => {
                        self.flash = Some((slot, at));
                        self.mark_used(slot);
                    }
                    None => {
                        self.change(Change::FocusInto { slot: from, task });
                        self.mark_used(from);
                    }
                },
                Action::Delete(id) => self.confirm_delete = Some(id),
                Action::AddSubtask(id, title) => {
                    self.new_task = Some(NewTaskDraft {
                        parent: Some(id),
                        title,
                        ..Default::default()
                    });
                }
                Action::StartPromote(note_id) => {
                    if let Some((id, note)) = self.api.board().note(note_id) {
                        let (title, body) = match split_title(&note.text) {
                            (Some(title), body) => (title, body.trim()),
                            (None, text) => (text, ""),
                        };
                        self.new_task = Some(NewTaskDraft {
                            parent: Some(id),
                            from_step: Some(note_id),
                            title: title.to_owned(),
                            notes: body.to_owned(),
                            ..Default::default()
                        });
                    }
                }
                Action::StartEdit(note_id) => {
                    self.editing = self.api.board().note(note_id).map(|(_, note)| NoteEdit {
                        note: note_id,
                        text: note.text.clone(),
                        focus_requested: false,
                    });
                }
                Action::StartMove(note) => {
                    self.moving = Some(NoteMove { note, target: None });
                }
                Action::SaveEdit => {
                    if let Some(edit) = self.editing.take() {
                        self.change(Change::EditNote {
                            note: edit.note,
                            text: edit.text,
                        });
                    }
                }
                Action::CancelEdit => self.editing = None,
            }
        }
    }

    fn mark_used(&mut self, slot: usize) {
        self.use_tick += 1;
        self.slot_used[slot] = self.use_tick;
    }

    /// Sends a change to the API, logging why it failed if it did.
    fn change(&mut self, change: Change) {
        if let Err(e) = self.api.apply(change) {
            eprintln!("its4tasks: {e}");
        }
        // Stop editing a comment that is gone.
        let board = self.api.board();
        self.editing.take_if(|e| board.find_note(e.note).is_none());
    }

    fn task_list(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                self.board_menu(ui);
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

        let shown = self.api.board().list_rows(&self.filter, self.hide_closed);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!(
                    "{} of {} tasks",
                    shown.len(),
                    self.api.board().tasks.len()
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
                if self.view == View::Log {
                    ui.add_space(8.0);
                    let hint = "Show every task in the log, or none";
                    let board = self.api.board();
                    let all = !board.tasks.iter().any(|t| self.log_hidden.contains(&t.id));
                    let mut on = all;
                    let label = Label::new(RichText::new("Show all").small())
                        .selectable(false)
                        .sense(Sense::click());
                    if ui.add(label).on_hover_text(hint).clicked() {
                        on = !on;
                    }
                    let color = ui.visuals().selection.bg_fill;
                    toggle_switch(ui, &mut on, color, "Show all").on_hover_text(hint);
                    if on != all {
                        self.log_hidden = if on {
                            HashSet::new()
                        } else {
                            board.tasks.iter().map(|t| t.id).collect()
                        };
                    }
                }
            });
        });
        ui.separator();

        let has_free_slot = self.api.board().has_free_slot();
        let lru = self.api.board().lru_slot(&self.slot_used);
        let mut replacing = None;
        let now = Utc::now();
        let log_mode = self.view == View::Log;
        // A task clicked in or out of the log, and whether it's now in.
        let mut log_toggle: Option<(Uuid, bool)> = None;
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            if self.api.board().tasks.is_empty() {
                ui.add_space(8.0);
                ui.weak("No tasks yet. Press + to create one.");
            }
            let guides = list_guides(&shown);
            for (i, &(id, depth)) in shown.iter().enumerate() {
                // Each root task's tree is its own group.
                if depth == 0 && i > 0 {
                    ui.add(egui::Separator::default().spacing(6.0));
                }
                let guides = &guides[i];
                let has_shown_child = shown.get(i + 1).is_some_and(|&(_, d)| d == depth + 1);
                let slot = self.api.board().slot_of(id);
                let has_subtasks = self.api.board().children(id).next().is_some();
                // Only shown while folds apply, i.e. without a filter.
                let hidden = self
                    .api
                    .board()
                    .task(id)
                    .filter(|t| t.folded && self.filter.trim().is_empty())
                    .map(|_| self.api.board().hidden_subtasks(id, &shown, self.hide_closed));
                let blocker = self
                    .api
                    .board()
                    .task(id)
                    .and_then(|t| blocked_text(self.api.board(), t, now));
                let next_steps = (self.api.board().state(id) == TaskState::Ready)
                    .then(|| self.api.board().task(id).map(next_steps_text))
                    .flatten();
                // Whether a task dragged over this row may become its subtask.
                let can_adopt = |dragged: Uuid| self.api.board().can_set_parent(dragged, Some(id));
                let hovered_payload =
                    DragAndDrop::payload::<Uuid>(ui.ctx()).filter(|d| can_adopt(**d));
                let Some(task) = self.api.board().task(id) else {
                    continue;
                };
                let is_subtask = task.parent.is_some();
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
                        tree_guides(ui, depth, guides, has_shown_child);
                        ui.add_space(depth as f32 * SUBTASK_INDENT);
                        let color = if task.done {
                            ui.visuals().weak_text_color()
                        } else {
                            freshness_color(task, now, ui.visuals())
                        };
                        let (bar, bar_response) =
                            ui.allocate_exact_size(Vec2::new(4.0, 16.0), Sense::hover());
                        ui.painter().rect_filled(bar, 2.0, color);
                        let mut hover = updated_text(task, now);
                        if let Some(text) = blocker.as_ref().or(next_steps.as_ref()) {
                            hover = format!("{text}\n{hover}");
                        }
                        bar_response.on_hover_text(hover);
                        let mut done = task.done;
                        if ui.checkbox(&mut done, "").changed() {
                            actions.push(Change::SetDone { task: id, done }.into());
                        }
                        // Kept for tasks without subtasks too, so titles line up.
                        let (arrow, _) =
                            ui.allocate_exact_size(Vec2::new(12.0, 16.0), Sense::hover());
                        let mut toggle_fold = false;
                        if has_subtasks {
                            let response = ui
                                .interact(arrow, Id::unique(("fold", id)), Sense::click())
                                .on_hover_text(if task.folded {
                                    "Unfold subtasks"
                                } else {
                                    "Fold subtasks"
                                });
                            toggle_fold |= response.clicked();
                            let color = if response.hovered() {
                                ui.visuals().strong_text_color()
                            } else {
                                ui.visuals().weak_text_color()
                            };
                            ui.painter().add(fold_arrow(arrow.center(), task.folded, color));
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if log_mode {
                                let shown = !self.log_hidden.contains(&id);
                                let (dot, response) =
                                    ui.allocate_exact_size(Vec2::splat(12.0), Sense::click());
                                let color = if response.hovered() {
                                    ui.visuals().strong_text_color()
                                } else if shown {
                                    ui.visuals().selection.bg_fill
                                } else {
                                    ui.visuals().weak_text_color()
                                };
                                if shown {
                                    ui.painter().circle_filled(dot.center(), 5.0, color);
                                } else {
                                    let stroke = Stroke::new(1.5, color);
                                    ui.painter().circle_stroke(dot.center(), 4.25, stroke);
                                }
                                let hint = if shown {
                                    "Shown in the log · click to hide it and its subtasks"
                                } else {
                                    "Hidden from the log · click to show it and its subtasks"
                                };
                                if response.on_hover_text(hint).clicked() {
                                    log_toggle = Some((id, !shown));
                                }
                            } else if slot.is_some() {
                                // Painted rather than a "●" glyph, which sits on the text
                                // baseline instead of the row's center.
                                let (dot, response) =
                                    ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                                let color = ui.visuals().selection.bg_fill;
                                ui.painter().circle_filled(dot.center(), 5.0, color);
                                response.on_hover_text("In focus");
                            } else {
                                let btn = ui.add(Button::new("→").small());
                                if has_free_slot {
                                    let btn = btn.on_hover_text("Bring into focus");
                                    if btn.clicked() {
                                        actions.push(Change::Focus { task: id }.into());
                                    }
                                } else if let Some(lru) = lru {
                                    // All slots taken: offer the least recently used
                                    // card, highlighted while the menu is open.
                                    let btn = btn.on_hover_text(
                                        "Bring into focus, replacing the least recently used card",
                                    );
                                    let menu = Popup::menu(&btn).show(|ui| {
                                        if ui.button("Replace highlighted task").clicked() {
                                            actions.push(
                                                Change::FocusInto {
                                                    slot: lru,
                                                    task: id,
                                                }
                                                .into(),
                                            );
                                        }
                                    });
                                    if menu.is_some() {
                                        replacing = Some(lru);
                                    }
                                }
                            }
                            // Always laid out, so titles don't reflow on hover.
                            task_menu(ui, id, is_subtask, row_hovered, actions);
                            if let Some(n) = hidden.filter(|n| *n > 0) {
                                let plural = if n == 1 { "" } else { "s" };
                                ui.label(RichText::new(n.to_string()).small().weak())
                                    .on_hover_text(format!("{n} subtask{plural} hidden"));
                            }
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                let mut text = RichText::new(&task.title);
                                if task.done {
                                    text = text.strikethrough().weak();
                                }
                                let drag_id = Id::unique(("task", id));
                                let dragged = ui.dnd_drag_source(drag_id, id, |ui| {
                                    ui.add(Label::new(text).truncate().selectable(false))
                                });
                                let mut response = dragged.response;
                                if has_subtasks {
                                    // Same id as the drag source, so egui merges the senses:
                                    // a press without moving is a click, else a drag.
                                    response = ui.interact(response.rect, drag_id, Sense::click());
                                    toggle_fold |= response.clicked();
                                }
                                let hint = if has_subtasks {
                                    "Click to fold or unfold · drag onto a card to focus, or onto a task to make it a subtask"
                                } else {
                                    "Drag onto a card to focus, or onto a task to make it a subtask"
                                };
                                let label = response.on_hover_text(hint);
                                label.context_menu(|ui| {
                                    task_menu_items(ui, id, is_subtask, actions);
                                });
                            });
                        });
                        if toggle_fold {
                            actions.push(Change::ToggleFold { task: id }.into());
                        }
                    })
                });
                let rect = shown.response.rect;
                let glow_color = if blocker.is_some() {
                    Some(blocked_glow_color(ui.visuals()))
                } else if next_steps.is_some() {
                    Some(next_glow_color(ui.visuals()))
                } else {
                    None
                };
                if let Some(color) = glow_color {
                    let shape = inner_glow(rect, ROW_RADIUS, ROW_GLOW_WIDTH, color);
                    ui.painter().set(glow, shape);
                }
                // Dropping a task onto a row makes it a subtask of that row.
                if hovered_payload.is_some() && ui.rect_contains_pointer(rect) {
                    let stroke = Stroke::new(2.0, ui.visuals().selection.bg_fill);
                    ui.painter()
                        .rect_stroke(rect, ROW_RADIUS, stroke, StrokeKind::Inside);
                    if ui.input(|i| i.pointer.any_released())
                        && let Some(dragged) = DragAndDrop::take_payload::<Uuid>(ui.ctx())
                    {
                        actions.push(Change::SetParent { task: *dragged, parent: Some(id) }.into());
                    }
                }
            }
        });
        if let Some((id, shown)) = log_toggle {
            log::set_shown(self.api.board(), &mut self.log_hidden, id, shown);
        }
        self.replacing = replacing;
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
        let colors = palette(&visuals);
        let now = Utc::now();
        let task_id = self.api.board().slots[slot];
        let task = task_id.and_then(|id| self.api.board().task(id));
        let stroke = match task {
            Some(task) if !task.done => Stroke::new(2.0, freshness_color(task, now, &visuals)),
            _ => Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
        };
        // Read from the whole board before the task is borrowed mutably below.
        let state = task_id.map(|id| self.api.board().state(id));
        let blocked = state == Some(TaskState::Blocked);
        let blocked_since = task_id.and_then(|id| self.api.board().blocked_since(id));
        let by_subtasks = task_id.is_some_and(|id| self.api.board().blocked_by_subtasks(id));
        let parent = task
            .and_then(|t| t.parent)
            .and_then(|p| self.api.board().task(p))
            .map(|p| (p.id, p.title.clone()));
        // Direct subtasks: id, title, done, and whether in focus.
        let subtasks: Vec<(Uuid, String, bool, bool)> = task_id
            .into_iter()
            .flat_map(|id| self.api.board().children(id))
            .map(|t| {
                let focused = self.api.board().slot_of(t.id).is_some();
                (t.id, t.title.clone(), t.done, focused)
            })
            .collect();
        let time = ui.input(|i| i.time);
        let has_task = task.is_some();
        let ready = state == Some(TaskState::Ready);
        // Titles of the subtasks this task's steps were promoted to, by subtask id.
        let promoted: Vec<(Uuid, String)> = task
            .iter()
            .flat_map(|t| &t.notes)
            .filter_map(|n| match n.next_step {
                Some(NextStep::Promoted { subtask, .. }) => {
                    let title = self.api.board().task(subtask)?.title.clone();
                    Some((subtask, title))
                }
                _ => None,
            })
            .collect();
        // The background (and glow) is painted into this slot once the card's rect
        // is known, so it stays behind the card's contents.
        let background = ui.painter().add(Shape::Noop);
        let frame = Frame::new()
            .inner_margin(Margin::same(CARD_MARGIN))
            .corner_radius(CARD_RADIUS)
            .stroke(stroke);
        let content = cell - Vec2::splat(2.0 * (f32::from(CARD_MARGIN) + stroke.width));

        let (zone, dropped) = ui.dnd_drop_zone::<Uuid, _>(frame, |ui| {
            ui.set_min_size(content);
            ui.set_max_size(content);
            let Some(task) = task_id.and_then(|id| self.api.board().task(id)) else {
                ui.centered_and_justified(|ui| {
                    ui.weak("Drop a task here or press → in the list");
                });
                return;
            };
            ui.horizontal(|ui| {
                let mut done = task.done;
                if ui.checkbox(&mut done, "").changed() {
                    actions.push(
                        Change::SetDone {
                            task: task.id,
                            done,
                        }
                        .into(),
                    );
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add(Button::new("✕").frame(false))
                        .on_hover_text("Send back to the list")
                        .clicked()
                    {
                        actions.push(Change::Unfocus { slot }.into());
                    }
                    task_menu(ui, task.id, task.parent.is_some(), true, actions);
                    let mut title = task.title.clone();
                    let edit = TextEdit::singleline(&mut title)
                        .font(TextStyle::Heading)
                        .frame(Frame::NONE)
                        .desired_width(f32::INFINITY);
                    if ui.add(edit).changed() {
                        actions.push(
                            Change::Rename {
                                task: task.id,
                                title,
                            }
                            .into(),
                        );
                    }
                });
            });
            ui.horizontal(|ui| {
                let state = state.unwrap_or(TaskState::Active);
                let label = ui.label(
                    RichText::new(state.label())
                        .small()
                        .strong()
                        .color(state_text_color(state, &visuals)),
                );
                if let Some(since) = blocked_since {
                    let mut text = format!("· for {}", short_duration(now - since));
                    if by_subtasks {
                        text.push_str(" · all subtasks blocked");
                    }
                    let color = state_text_color(state, &visuals);
                    let hover = format!("Blocked since {}", local_time(since));
                    label.on_hover_text(&hover);
                    ui.label(RichText::new(text).small().color(color))
                        .on_hover_text(hover);
                }
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
                // Before the parent, so the parent's title is what truncates.
                if !subtasks.is_empty() {
                    ui.label(RichText::new("·").small().weak());
                    let label = format!("Subtasks ({})", subtasks.len());
                    let response = link_label(ui, "subtasks", &label);
                    Popup::menu(&response).show(|ui| {
                        ui.set_max_width(320.0);
                        for (id, title, done, focused) in &subtasks {
                            let mut text = RichText::new(title);
                            if *done {
                                text = text.strikethrough().weak();
                            }
                            let mut button = Button::new(text).truncate();
                            if *focused {
                                button =
                                    button.shortcut_text(RichText::new("in focus").small().weak());
                            }
                            if ui.add(button).clicked() {
                                actions.push(Action::Navigate {
                                    from: slot,
                                    task: *id,
                                    at: time,
                                });
                            }
                        }
                    });
                }
                if let Some((id, title)) = &parent {
                    ui.label(RichText::new("·").small().weak());
                    let response = link_label(ui, "parent", &format!("↳ {title}"))
                        .on_hover_text(format!("Subtask of {title} · click to go to it"));
                    if response.clicked() {
                        actions.push(Action::Navigate {
                            from: slot,
                            task: *id,
                            at: time,
                        });
                    }
                }
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
                let error = &mut self.comment_error[slot];
                if error.as_ref().is_some_and(|(id, _)| *id != task.id) {
                    *error = None;
                }
                if let Some((_, text)) = error {
                    ui.label(RichText::new(text.as_str()).small().color(colors.blocker));
                }
                let submit = ui.memory(|m| m.has_focus(input_id))
                    && ui.input_mut(|i| {
                        !i.modifiers.shift && i.consume_key(Modifiers::NONE, Key::Enter)
                    });
                // A fully typed command shows in the picker what Enter will do.
                let preview = task.parse_input(draft).preview();
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
                            // Keeps the column's width when the label is short.
                            ui.set_min_width(KIND_PICKER_WIDTH);
                            let (selected, hint) = match preview {
                                Some(Command::Subtask) => (
                                    RichText::new("Subtask").color(colors.command),
                                    "Open the new subtask dialog",
                                ),
                                command => {
                                    let k = command.and_then(Command::kind).unwrap_or(*kind);
                                    let text = RichText::new(k.label());
                                    (text.color(kind_color(k, &visuals)), kind_hint(k))
                                }
                            };
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
                                .on_hover_text(hint);
                        },
                    );
                    let hint = match *kind {
                        MessageKind::Comment => "Add a comment… (Enter to post, / for commands)",
                        MessageKind::Star => "Something worth remembering… (Enter to post)",
                        MessageKind::Next => "What's the next step?… (Enter to post)",
                        MessageKind::Block => "Describe what you're waiting for… (Enter to post)",
                        MessageKind::Unblock => "What unblocked it?… (Enter to post)",
                        MessageKind::Close => "Closing note… (Enter to post)",
                    };
                    // Colours a leading `/command`: blue if Enter can run it, red if not.
                    let task = &*task;
                    let mut layouter = |ui: &egui::Ui, text: &dyn TextBuffer, wrap_width: f32| {
                        let text = text.as_str();
                        let parsed = task.parse_input(text);
                        let token = parsed.token_len.unwrap_or(0);
                        let color = if parsed.result.is_ok() {
                            colors.command
                        } else {
                            colors.blocker
                        };
                        let font = TextStyle::Body.resolve(ui.style());
                        let normal = ui.visuals().text_color();
                        let mut job = LayoutJob::default();
                        job.append(&text[..token], 0.0, TextFormat::simple(font.clone(), color));
                        job.append(&text[token..], 0.0, TextFormat::simple(font, normal));
                        job.wrap.max_width = wrap_width;
                        // Dropping it would make the cursor jump back while typing.
                        job.keep_trailing_whitespace = true;
                        ui.fonts_mut(|f| f.layout_job(job))
                    };
                    let edit = ScrollArea::vertical()
                        .id_salt(("comment_input_scroll", slot))
                        .max_height(max_height)
                        .show(ui, |ui| {
                            TextEdit::multiline(draft)
                                .id(input_id)
                                .hint_text(hint)
                                .background_color(comment_input_bg(&visuals))
                                .margin(COMMENT_INPUT_MARGIN)
                                .desired_rows(2)
                                .desired_width(f32::INFINITY)
                                .layouter(&mut layouter)
                                .show(ui)
                        })
                        .inner;
                    let response = &edit.response.response;
                    if response.changed() {
                        *error = None;
                    }
                    // Explain an invalid command on hover, unless it's still
                    // being typed.
                    let parsed = task.parse_input(draft);
                    if let (Some(len), Err(e)) = (parsed.token_len, &parsed.result)
                        && !parsed.partial
                    {
                        let chars = draft[..len].chars().count();
                        let galley = &edit.galley;
                        let token = galley
                            .pos_from_cursor(CCursor::new(0))
                            .union(galley.pos_from_cursor(CCursor::new(chars)))
                            .translate(edit.galley_pos.to_vec2());
                        ui.interact(token, input_id.with("command"), Sense::hover())
                            .on_hover_text(e.to_string());
                    }
                    let height = response.rect.height().clamp(min_height, max_height);
                    if (height - row_height).abs() > 0.5 {
                        ui.data_mut(|d| d.insert_temp(height_id, height));
                        ui.ctx().request_discard("comment box resized");
                    }
                });
                if submit && !draft.trim().is_empty() {
                    match task.parse_input(draft).result {
                        Ok(Submit::Post(k, text)) => {
                            let post = Change::post(task.id, text.to_owned(), k.unwrap_or(*kind));
                            actions.push(post.into());
                            draft.clear();
                            *kind = MessageKind::Comment;
                        }
                        Ok(Submit::SetKind(k)) => {
                            *kind = k;
                            draft.clear();
                        }
                        Ok(Submit::NewSubtask(title)) => {
                            actions.push(Action::AddSubtask(task.id, title.to_owned()));
                            draft.clear();
                        }
                        Err(e) => *error = Some((task.id, e.to_string())),
                    }
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
                            for note in &task.notes {
                                let edit = self.editing.as_mut().filter(|e| e.note == note.id);
                                let can_block = task.can_block();
                                let promoted_to = match note.next_step {
                                    Some(NextStep::Promoted { subtask, .. }) => promoted
                                        .iter()
                                        .find(|(s, _)| *s == subtask)
                                        .map(|(_, title)| title.as_str()),
                                    _ => None,
                                };
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
                                            comment(
                                                ui,
                                                note,
                                                hovered,
                                                can_block,
                                                promoted_to,
                                                edit,
                                                now,
                                                actions,
                                            );
                                        });
                                        let glow_color = if note.is_open_blocker() {
                                            Some(blocked_glow_color(&visuals))
                                        } else if note.is_open_step() {
                                            Some(next_glow_color(&visuals))
                                        } else {
                                            None
                                        };
                                        if let Some(color) = glow_color {
                                            ui.painter().set(
                                                glow,
                                                inner_glow(
                                                    shown.response.rect,
                                                    COMMENT_RADIUS,
                                                    COMMENT_GLOW_WIDTH,
                                                    color,
                                                ),
                                            );
                                        }
                                    })
                                    .response;
                                entry.context_menu(|ui| {
                                    comment_menu_items(ui, note, can_block, actions);
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
            let glow_color = if blocked {
                Some(blocked_glow_color(&visuals))
            } else if ready {
                Some(next_glow_color(&visuals))
            } else {
                None
            };
            if let Some(color) = glow_color {
                let inside = rect.shrink(stroke.width);
                shapes.push(inner_glow(
                    inside,
                    CARD_RADIUS - stroke.width,
                    CARD_GLOW_WIDTH,
                    color,
                ));
            }
            ui.painter().set(background, Shape::Vec(shapes));
            // Over the contents: the flash after navigating here, fading out,
            // or the steady highlight of the card a replace would take.
            let flash = self
                .flash
                .filter(|(s, _)| *s == slot)
                .map(|(_, since)| time - since)
                .filter(|age| *age < FLASH_SECS);
            let alpha = match flash {
                Some(age) => {
                    ui.ctx().request_repaint();
                    Some(1.0 - (age / FLASH_SECS) as f32)
                }
                None if self.replacing == Some(slot) => Some(1.0),
                None => None,
            };
            if let Some(alpha) = alpha {
                let color = visuals.selection.bg_fill.gamma_multiply(alpha);
                ui.painter().rect_stroke(
                    rect,
                    CARD_RADIUS,
                    Stroke::new(HIGHLIGHT_WIDTH, color),
                    StrokeKind::Inside,
                );
            }
            if zone.response.contains_pointer() && ui.input(|i| i.pointer.any_pressed()) {
                self.mark_used(slot);
            }
        }
        if self
            .flash
            .is_some_and(|(s, since)| s == slot && time - since >= FLASH_SECS)
        {
            self.flash = None;
        }
        if let Some(id) = dropped {
            actions.push(Change::FocusInto { slot, task: *id }.into());
        }
    }

    /// The "⋯" menu next to "+": importing, and switching boards.
    fn board_menu(&mut self, ui: &mut egui::Ui) {
        let button = Button::new(RichText::new(" ⋯ ").strong());
        egui::containers::menu::MenuButton::from_button(button)
            .ui(ui, |ui| {
                let import = ui.add_enabled(
                    self.picking.is_none(),
                    Button::new("Import its4tasks.json…"),
                );
                if import.clicked() {
                    self.pick_import(ui.ctx());
                }
                let export =
                    ui.add_enabled(self.saving.is_none(), Button::new("Export its4tasks.json…"));
                if export
                    .on_hover_text("Save your whole board to a file")
                    .clicked()
                {
                    self.export(ui.ctx());
                }
                if self.switchable && ui.button("Switch board…").clicked() {
                    self.request = Some(Request::SwitchBoard);
                }
            })
            .0
            .on_hover_text("More");
    }

    fn view_bar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.view, View::Board, "Board");
            ui.selectable_value(&mut self.view, View::Log, "Log");
            if self.view == View::Board {
                ui.separator();
                let slots = self.api.board().slots;
                let clear = ui
                    .add_enabled(
                        slots.iter().any(Option::is_some),
                        Button::new("Clear board"),
                    )
                    .on_hover_text("Send every card back to the list")
                    .on_disabled_hover_text("The board is already empty");
                if clear.clicked() {
                    for slot in (0..SLOT_COUNT).filter(|&s| slots[s].is_some()) {
                        actions.push(Change::Unfocus { slot }.into());
                    }
                }
            }
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
                ComboBox::from_id_salt("log_filter")
                    .selected_text(self.log_filter.label())
                    .show_ui(ui, |ui| {
                        for filter in [Filter::All, Filter::Starred, Filter::Pending] {
                            ui.selectable_value(&mut self.log_filter, filter, filter.label());
                        }
                    })
                    .response
                    .on_hover_text(
                        "Starred: starred comments\n\
                         Pending: open blockers and next steps, on the day they were set",
                    );
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
                // Refusals while a token is renewed are retried quietly; only
                // a lasting one is shown. Unsent changes are kept meanwhile.
                let problem = if self.api.signed_out() {
                    #[cfg(target_arch = "wasm32")]
                    if ui.button("Sign in again").clicked() {
                        self.keep_unsent();
                        crate::client::sign_in_again();
                    }
                    Some("Signed out. Your unsent changes are kept.".to_owned())
                } else if self.api.unauthorized() {
                    None
                } else {
                    match (self.api.error(), &self.file_problem, self.api.notice()) {
                        (Some(e), _, _) => Some(format!("Can't reach the server: {e}")),
                        (None, Some(e), _) => Some(e.clone()),
                        (None, None, notice) => notice.map(str::to_owned),
                    }
                };
                if let Some(problem) = problem {
                    let text = RichText::new(problem).color(palette(ui.visuals()).blocker);
                    ui.add(Label::new(text).truncate());
                }
            });
        });
        ui.add_space(6.0);
    }

    fn log_view(&mut self, ui: &mut egui::Ui) {
        let pages = log::build(
            self.api.board(),
            self.log_period,
            self.log_filter,
            &self.log_hidden,
            &Local,
        );
        let board = self.api.board();
        let none_shown = board.tasks.iter().all(|t| self.log_hidden.contains(&t.id));
        let time_format = match self.log_period {
            Period::Day => "%H:%M",
            Period::Week => "%a %H:%M",
        };
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            if pages.is_empty() {
                ui.add_space(8.0);
                ui.weak(if none_shown && !board.tasks.is_empty() {
                    "No tasks selected. Pick some in the list."
                } else {
                    match self.log_filter {
                        Filter::All => "Nothing logged yet.",
                        Filter::Starred => "No starred comments yet. Star a comment from its card.",
                        Filter::Pending => "No open blockers or next steps.",
                    }
                });
            }
            for page in &pages {
                ui.add_space(8.0);
                ui.label(RichText::new(log_heading(page, self.log_period)).heading());
                ui.separator();
                for group in &page.groups {
                    ui.add_space(6.0);
                    log_group(ui, group, page.start, false, time_format);
                }
            }
            ui.add_space(8.0);
        });
    }

    fn delete_modal(&mut self, ctx: &egui::Context) {
        let Some(task) = self.confirm_delete.and_then(|id| self.api.board().task(id)) else {
            self.confirm_delete = None;
            return;
        };
        let id = task.id;
        let prompt = delete_prompt(task, self.api.board().descendants(id).len());
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
            self.change(Change::DeleteTask { task: id });
            self.confirm_delete = None;
        } else if cancel || modal.should_close() {
            self.confirm_delete = None;
        }
    }

    /// Pick the task a comment moves to: the open tasks as in the task list,
    /// greyed out where it can't go, then the comments that will move.
    fn move_modal(&mut self, ctx: &egui::Context) {
        let Some(moving) = &mut self.moving else {
            return;
        };
        let note = moving.note;
        let found = self.api.board().find_note(note);
        let Some(((from, idx), source)) =
            found.and_then(|(from, idx)| Some(((from, idx), self.api.board().task(from)?)))
        else {
            self.moving = None;
            return;
        };
        let rows = self.api.board().list_rows("", true);
        let mut confirm = false;
        let mut cancel = false;
        let modal = Modal::new(Id::unique("move_note_modal")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading("Move comment");
            ui.add(Label::new(RichText::new(format!("From: {}", source.title)).weak()).truncate());
            ui.add_space(8.0);
            ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                for (id, depth) in rows {
                    let Some(task) = self.api.board().task(id) else {
                        continue;
                    };
                    let allowed = self.api.board().can_move_note(from, idx, id);
                    ui.horizontal(|ui| {
                        ui.add_space(depth as f32 * SUBTASK_INDENT);
                        let selected = moving.target == Some(id);
                        let mut title = RichText::new(&task.title);
                        if task.done {
                            title = title.strikethrough();
                        }
                        // The growing atom keeps the title on the left, so
                        // subtasks line up under their indent.
                        let row = Button::selectable(selected, (title, Atom::grow()))
                            .truncate()
                            .min_size(Vec2::new(ui.available_width(), 0.0));
                        let response = ui.add_enabled(allowed.is_ok(), row);
                        let response = match allowed {
                            Err(e) => response.on_disabled_hover_text(e.to_string()),
                            Ok(_) => response,
                        };
                        if response.clicked() {
                            moving.target = Some(id);
                        }
                    });
                }
            });
            ui.separator();
            ui.label(RichText::new("Will move:").small().weak());
            for i in source.linked_notes(idx) {
                let note = &source.notes[i];
                ui.horizontal(|ui| {
                    ui.label(RichText::new(local_time(note.at)).small().weak());
                    ui.add(Label::new(moved_note_job(note, ui)).truncate());
                });
            }
            ui.add_space(12.0);
            // Right-aligned; added right to left, so Cancel comes first.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
                let can_move = moving.target.is_some();
                if ui.add_enabled(can_move, Button::new("Move")).clicked() {
                    confirm = true;
                }
            });
        });
        if confirm && let Some(to) = moving.target {
            self.change(Change::MoveNote { note, to });
            self.moving = None;
        } else if cancel || modal.should_close() {
            self.moving = None;
        }
    }

    fn new_task_modal(&mut self, ctx: &egui::Context) {
        let has_free_slot = self.api.board().has_free_slot();
        let Some(draft) = &mut self.new_task else {
            return;
        };
        let parent_title = draft
            .parent
            .and_then(|id| self.api.board().task(id))
            .map(|t| t.title.clone());
        let promoting = draft.from_step.is_some();
        let mut submit = false;
        let mut cancel = false;
        let modal = Modal::new(Id::unique("new_task_modal")).show(ctx, |ui| {
            ui.set_width(420.0);
            match &parent_title {
                Some(title) => {
                    ui.heading(if promoting {
                        "Promote to subtask"
                    } else {
                        "New subtask"
                    });
                    ui.add(
                        Label::new(RichText::new(format!("Subtask of: {title}")).weak()).truncate(),
                    );
                }
                None => {
                    ui.heading("New task");
                }
            }
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
                let create = if promoting { "Promote" } else { "Create" };
                if ui.add_enabled(can_create, Button::new(create)).clicked() {
                    submit = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });

        if submit && !draft.title.trim().is_empty() {
            let draft = self.new_task.take().unwrap_or_default();
            let title = draft.title.trim().to_owned();
            self.change(match (draft.parent, draft.from_step) {
                (Some(_), Some(note)) => {
                    Change::promote_step(note, title, draft.notes, draft.add_to_focus)
                }
                _ => Change::add_task(title, draft.notes, draft.parent, draft.add_to_focus),
            });
        } else if cancel || modal.should_close() {
            self.new_task = None;
        }
    }
}

impl eframe::App for TodoApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.api.poll();
        self.api.refresh();
        self.poll_import();
        self.poll_export();
        if !self.api.loaded() {
            self.loading(ui);
            return;
        }
        if !self.restored {
            self.restore_unsent();
        } else if self.kept.elapsed() >= Duration::from_secs(1) {
            self.keep_unsent();
        }
        if !self.api.started() {
            self.landing(ui);
            self.apply(Vec::new());
            return;
        }
        let mut actions = Vec::new();
        Panel::right("task_list")
            .resizable(true)
            .default_size(320.0)
            .min_size(240.0)
            .show(ui, |ui| self.task_list(ui, &mut actions));
        CentralPanel::default().show(ui, |ui| {
            self.view_bar(ui, &mut actions);
            match self.view {
                View::Board => self.cards(ui, &mut actions),
                View::Log => self.log_view(ui),
            }
        });
        self.new_task_modal(&ui.ctx().clone());
        self.delete_modal(&ui.ctx().clone());
        self.move_modal(&ui.ctx().clone());
        self.apply(actions);
        if self.flash.is_some() {
            // Starts the flash now; the card keeps repainting while it runs.
            ui.ctx().request_repaint();
        }
        // Keep the freshness colors drifting while the app sits idle.
        // `refresh` fetches the board too, but only once a minute.
        ui.ctx().request_repaint_after(Duration::from_secs(2));
    }

    /// Gives the last changes a moment to reach the server.
    #[cfg(not(target_arch = "wasm32"))]
    fn on_exit(&mut self) {
        if !self.api.flush(Duration::from_secs(2)) {
            eprintln!("its4tasks: closing with changes the server hasn't confirmed");
        }
    }
}
