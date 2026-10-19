use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use eframe::egui::{
    self, Align, Button, CentralPanel, Color32, Frame, Id, Key, Label, Layout, Margin, Modal,
    Modifiers, Panel, Rect, RichText, ScrollArea, Sense, Stroke, TextEdit, TextStyle, UiBuilder,
    Vec2,
};

use crate::model::{Board, SLOT_COUNT, Task, matches_filter, staleness};
use crate::storage;

const CARD_GAP: f32 = 12.0;
const CARD_MARGIN: i8 = 14;

const FRESH: Color32 = Color32::from_rgb(76, 175, 80);
const AGING: Color32 = Color32::from_rgb(230, 190, 40);
const STALE: Color32 = Color32::from_rgb(220, 70, 60);

/// Green → yellow → red by time since the last update; `None` for finished tasks.
fn freshness_color(task: &Task, now: DateTime<Utc>) -> Option<Color32> {
    if task.done {
        return None;
    }
    let t = staleness(now - task.updated_at);
    Some(if t < 0.5 {
        FRESH.lerp_to_gamma(AGING, t * 2.0)
    } else {
        AGING.lerp_to_gamma(STALE, (t - 0.5) * 2.0)
    })
}

fn local_time(at: DateTime<Utc>) -> String {
    at.with_timezone(&Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

fn updated_text(task: &Task, now: DateTime<Utc>) -> String {
    let age = now - task.updated_at;
    let ago = match (age.num_days(), age.num_hours(), age.num_minutes()) {
        (d, _, _) if d >= 1 => format!("{d} day{} ago", if d == 1 { "" } else { "s" }),
        (_, h, _) if h >= 1 => format!("{h} hour{} ago", if h == 1 { "" } else { "s" }),
        (_, _, m) if m >= 1 => format!("{m} min ago"),
        _ => "just now".to_owned(),
    };
    format!("Updated {ago} · {}", local_time(task.updated_at))
}

#[derive(Default)]
struct NewTaskDraft {
    title: String,
    notes: String,
    add_to_focus: bool,
    focus_requested: bool,
}

/// Changes requested while drawing, applied once drawing is done so we don't
/// mutate the board while iterating over it.
enum Action {
    Focus(u64),
    FocusInto(usize, u64),
    Unfocus(usize),
    Delete(u64),
    DeleteNote(u64, usize),
}

pub struct TodoApp {
    board: Board,
    filter: String,
    new_task: Option<NewTaskDraft>,
    /// Unsent comment text for each focus card.
    comment_drafts: [String; SLOT_COUNT],
    dirty: bool,
}

impl TodoApp {
    pub fn new() -> Self {
        Self {
            board: storage::load(),
            filter: String::new(),
            new_task: None,
            comment_drafts: Default::default(),
            dirty: false,
        }
    }

    fn apply(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Focus(id) => {
                    self.board.focus(id);
                }
                Action::FocusInto(slot, id) => self.board.focus_into(slot, id),
                Action::Unfocus(slot) => self.board.unfocus(slot),
                Action::Delete(id) => self.board.delete_task(id),
                Action::DeleteNote(id, idx) => {
                    if let Some(task) = self.board.task_mut(id) {
                        task.delete_note(idx);
                    }
                }
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
            .filter(|t| matches_filter(t, &self.filter))
            .map(|t| t.id)
            .collect();
        ui.label(
            RichText::new(format!(
                "{} of {} tasks",
                shown.len(),
                self.board.tasks.len()
            ))
            .small()
            .weak(),
        );
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
                ui.horizontal(|ui| {
                    let color =
                        freshness_color(task, now).unwrap_or(ui.visuals().weak_text_color());
                    let (bar, bar_response) =
                        ui.allocate_exact_size(Vec2::new(4.0, 16.0), Sense::hover());
                    ui.painter().rect_filled(bar, 2.0, color);
                    bar_response.on_hover_text(updated_text(task, now));
                    if ui.checkbox(&mut task.done, "").changed() {
                        task.touch(now);
                        self.dirty = true;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if slot.is_some() {
                            ui.label(RichText::new("●").color(ui.visuals().selection.bg_fill))
                                .on_hover_text("In focus");
                        } else {
                            let btn = ui
                                .add_enabled(has_free_slot, Button::new("→").small())
                                .on_hover_text("Bring into focus")
                                .on_disabled_hover_text("All 4 focus slots are taken");
                            if btn.clicked() {
                                actions.push(Action::Focus(id));
                            }
                        }
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
                                if ui.button("Delete").clicked() {
                                    actions.push(Action::Delete(id));
                                }
                            });
                        });
                    });
                });
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
        let stroke = match task_id.and_then(|id| self.board.task(id)) {
            Some(task) => match freshness_color(task, now) {
                Some(color) => Stroke::new(2.0, color),
                None => Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
            },
            None => Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
        };
        let frame = Frame::new()
            .inner_margin(Margin::same(CARD_MARGIN))
            .corner_radius(10.0)
            .fill(if task_id.is_some() {
                visuals.extreme_bg_color
            } else {
                Color32::TRANSPARENT
            })
            .stroke(stroke);
        let content = cell - Vec2::splat(2.0 * (f32::from(CARD_MARGIN) + stroke.width));

        let (_, dropped) = ui.dnd_drop_zone::<u64, _>(frame, |ui| {
            ui.set_min_size(content);
            ui.set_max_size(content);
            let Some(task) = task_id.and_then(|id| self.board.task_mut(id)) else {
                ui.centered_and_justified(|ui| {
                    ui.weak("Drop a task here or press → in the list");
                });
                return;
            };
            ui.horizontal(|ui| {
                if ui.checkbox(&mut task.done, "").changed() {
                    task.touch(now);
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
            ui.separator();

            // Lay out bottom-up so the comment box stays pinned to the card's
            // bottom edge and the comment log takes whatever height is left.
            ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                // Enter posts, Shift+Enter adds a newline.
                let input_id = ui.make_persistent_id(("comment_input", slot));
                let draft = &mut self.comment_drafts[slot];
                let submit = ui.memory(|m| m.has_focus(input_id))
                    && ui.input_mut(|i| {
                        !i.modifiers.shift && i.consume_key(Modifiers::NONE, Key::Enter)
                    });
                ui.add(
                    TextEdit::multiline(draft)
                        .id(input_id)
                        .hint_text("Add a comment… (Enter to post)")
                        .background_color(visuals.faint_bg_color)
                        .desired_rows(2)
                        .desired_width(f32::INFINITY),
                );
                if submit && !draft.trim().is_empty() {
                    task.add_note(draft, now);
                    draft.clear();
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
                                let entry = ui
                                    .scope(|ui| {
                                        ui.label(RichText::new(local_time(note.at)).small().weak());
                                        ui.add(Label::new(&note.text).wrap());
                                    })
                                    .response
                                    .interact(Sense::click());
                                entry.context_menu(|ui| {
                                    if ui.button("Delete").clicked() {
                                        actions.push(Action::DeleteNote(task.id, idx));
                                    }
                                });
                                ui.add_space(6.0);
                            }
                        });
                    });
            });
        });
        if let Some(id) = dropped {
            actions.push(Action::FocusInto(slot, *id));
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
        CentralPanel::default().show(ui, |ui| self.cards(ui, &mut actions));
        self.new_task_modal(&ui.ctx().clone());
        self.apply(actions);
        self.save_if_dirty();
        // Keep the freshness colors drifting while the app sits idle.
        ui.ctx().request_repaint_after(Duration::from_secs(60));
    }

    fn on_exit(&mut self) {
        self.save_if_dirty();
    }
}
