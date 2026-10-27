//! Templates & recipes in the app: the template picker and recipe menu on a
//! meeting, recipe results (Copy / Add to Note), and the Settings → Templates
//! and Recipes pages with their editor. Everything is stored as Markdown files
//! by `stenly_core::library`.

use crate::icons::{self, icon};
use crate::main_window::{MainWindow, Page};
use crate::text_input::TextInput;
use crate::theme;
use crate::ui::{self, Btn, MenuEntry};
use gpui::{AnyElement, App, ClipboardItem, Context, Entity, FontWeight, IntoElement, ParentElement, SharedString, Styled, Window, div, prelude::*, px};
use std::collections::HashSet;
use std::path::PathBuf;
use stenly_core::library::{Entry, Kind, Library, Origin};
use stenly_core::recipe::{self, RecipeResult};

#[derive(Default)]
pub struct LibraryState {
    /// Recipe runs in flight: (session dir, recipe id).
    running: Vec<(PathBuf, String)>,
    /// Last failure per (session dir, recipe id).
    errors: Vec<(PathBuf, String, String)>,
    /// Result cards the user opened or closed. The newest result starts open,
    /// the others as one-line headers; a toggle flips that.
    toggled: HashSet<(PathBuf, String)>,
    /// A note being rewritten with a new template.
    regenerating: Option<PathBuf>,
    /// The library item being edited (Settings → Templates/Recipes).
    editor: Option<Editor>,
}

pub struct Editor {
    kind: Kind,
    /// None while creating a new one.
    id: Option<String>,
    origin: Origin,
    name: Entity<TextInput>,
    description: Entity<TextInput>,
    /// Templates: `match:` keywords. Recipes: note heading.
    extra: Entity<TextInput>,
    body: Entity<TextInput>,
    error: Option<String>,
}

fn lib(kind: Kind) -> Library {
    match kind {
        Kind::Template => Library::templates(),
        Kind::Recipe => Library::recipes(),
    }
}

fn noun(kind: Kind) -> &'static str {
    match kind {
        Kind::Template => "Template",
        Kind::Recipe => "Recipe",
    }
}

/// A pop-up/pull-down button with owned labels that opens a native menu.
fn menu_button(
    id: impl Into<gpui::ElementId>,
    leading: Option<icons::Sym>,
    label: impl Into<SharedString>,
    items: Vec<String>,
    selected: Option<usize>,
    on_pick: impl Fn(usize, &mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let on_pick = std::rc::Rc::new(on_pick);
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(6.))
        .h(px(24.))
        .pl(px(if leading.is_some() { 8. } else { 10. }))
        .pr(px(5.))
        .rounded(px(6.))
        .bg(theme::button())
        .flex_shrink_0()
        .text_size(px(theme::BODY))
        .text_color(theme::text())
        .whitespace_nowrap()
        .active(|s| s.bg(theme::button_hover()))
        .when_some(leading, |d, s| d.child(icon(s, 12., theme::text())))
        .child(label.into())
        .child(icon(icons::CHEVRON_UPDOWN, 10., theme::muted()))
        .on_click(move |e, window, cx| {
            // NSMenu runs a nested event loop: open it with nothing borrowed.
            let pos = e.mouse_position().unwrap_or_else(|| window.mouse_position());
            let (items, on_pick) = (items.clone(), on_pick.clone());
            let view = ui::native_view(window);
            window
                .spawn(cx, async move |cx| {
                    let labels: Vec<&str> = items.iter().map(String::as_str).collect();
                    if let Some(i) = ui::menu_in_view(view, &labels, selected, pos) {
                        let _ = cx.update(|_, cx| on_pick(i, cx));
                    }
                })
                .detach();
        })
}

fn ago(t: chrono::DateTime<chrono::Local>) -> String {
    let s = (chrono::Local::now() - t).num_seconds().max(0);
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86_399 => format!("{} h ago", s / 3600),
        _ => t.format("%b %-d").to_string(),
    }
}

// ---------------------------------------------------------------- Markdown

/// `**bold**` → styled text without the markers.
fn inline(text: &str, size: f32, color: gpui::Rgba) -> AnyElement {
    let mut plain = String::new();
    let mut bold = Vec::new();
    let mut rest = text;
    while let Some(a) = rest.find("**") {
        let after = &rest[a + 2..];
        let Some(b) = after.find("**") else { break };
        plain.push_str(&rest[..a]);
        let start = plain.len();
        plain.push_str(&after[..b]);
        bold.push((start..plain.len(), gpui::HighlightStyle { font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() }));
        rest = &after[b + 2..];
    }
    plain.push_str(rest);
    let plain = plain.replace('`', "");
    // byte ranges are still valid only if no backticks were removed before them
    let styled = if text.contains('`') { gpui::StyledText::new(plain) } else { gpui::StyledText::new(plain).with_highlights(bold) };
    div().flex_1().min_w_0().text_size(px(size)).line_height(px(size * 1.5)).text_color(color).child(styled).into_any_element()
}

/// Light Markdown for recipe results: headings, bullets, numbered lists,
/// checkboxes and paragraphs.
pub fn markdown(text: &str) -> AnyElement {
    let mut out = div().flex().flex_col().gap(px(2.));
    let mut fence = false;
    let mut first = true;
    for raw in text.lines() {
        let line = raw.trim_end();
        if line.trim_start().starts_with("```") {
            fence = !fence;
            continue;
        }
        if fence {
            out = out.child(div().pl(px(8.)).text_size(px(theme::CALLOUT)).font_family("Menlo").text_color(theme::muted()).child(line.to_string()));
            continue;
        }
        let t = line.trim_start();
        let indent = (line.len() - t.len()) as f32 / 2.0 * 9.;
        if t.is_empty() {
            out = out.child(div().h(px(6.)));
        } else if let Some(h) = t.strip_prefix("#### ").or(t.strip_prefix("### ")).or(t.strip_prefix("## ")).or(t.strip_prefix("# ")) {
            out = out.child(div().pt(px(if first { 0. } else { 8. })).text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(h.replace("**", "")));
        } else if let Some(task) = t.strip_prefix("- [ ] ").or(t.strip_prefix("- [x] ")).or(t.strip_prefix("* [ ] ")) {
            out = out.child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(9.))
                    .pl(px(indent))
                    .py(px(1.))
                    .child(div().mt(px(3.5)).size(px(13.)).flex_shrink_0().rounded(px(3.5)).border_1().border_color(theme::faint()).bg(theme::inset()))
                    .child(inline(task, 14., theme::text())),
            );
        } else if let Some(b) = t.strip_prefix("- ").or(t.strip_prefix("* ")).or(t.strip_prefix("• ")) {
            let dot = if indent > 0. { div().mt(px(8.)).size(px(5.)).rounded_full().border_1().border_color(theme::muted()) } else { div().mt(px(8.)).size(px(5.)).rounded_full().bg(theme::muted()) };
            out = out.child(div().flex().items_start().gap(px(10.)).pl(px(indent)).py(px(1.)).child(dot.flex_shrink_0()).child(inline(b, 14., theme::text())));
        } else if let Some((n, rest)) = t.split_once(". ").filter(|(n, _)| !n.is_empty() && n.len() <= 3 && n.chars().all(|c| c.is_ascii_digit())) {
            out = out.child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(6.))
                    .pl(px(indent))
                    .py(px(1.))
                    .child(div().min_w(px(16.)).flex_shrink_0().text_size(px(14.)).line_height(px(21.)).text_color(theme::muted()).child(format!("{n}.")))
                    .child(inline(rest, 14., theme::text())),
            );
        } else if t == "---" || t == "***" {
            out = out.child(div().my(px(6.)).h(px(1.)).bg(theme::divider()));
        } else {
            out = out.child(div().pl(px(indent)).child(inline(t, 14., theme::text())));
        }
        first = false;
    }
    out.into_any_element()
}

// ---------------------------------------------------------------- the meeting view

impl MainWindow {
    /// The read view for an open meeting, with template + recipe tools.
    pub(crate) fn meeting_body(&mut self, m: &crate::meeting_view::MeetingDetail, cx: &mut Context<Self>) -> AnyElement {
        // Dev: STENLY_RUN_RECIPE=<id> / STENLY_REGENERATE=<template id|auto> act on the
        // open meeting as the menus would (end-to-end tests without clicking).
        static DEV: std::sync::Once = std::sync::Once::new();
        DEV.call_once(|| {
            if let Ok(id) = std::env::var("STENLY_RUN_RECIPE") {
                self.run_recipe(m.dir.clone(), id, cx);
            }
            if let Ok(t) = std::env::var("STENLY_REGENERATE") {
                self.regenerate(m.dir.clone(), (t != "auto").then_some(t), cx);
            }
        });
        // A regenerate finished once the controller has nothing left to do.
        if self.library.regenerating.is_some() && !self.controller().read(cx).is_processing() {
            self.library.regenerating = None;
        }
        // Destinations: "sent to …" line under the speakers (same slot, below the title).
        let sent = crate::destinations_ui::status_strip(&self.dest_ui, &m.dir, &m.meta, cx);
        let speakers = match (self.speaker_strip(cx), sent) {
            (Some(a), Some(b)) => Some(gpui::div().flex().flex_col().child(a).child(b).into_any_element()),
            (a, b) => a.or(b),
        };
        let tools = self.meeting_tools(m, cx);
        let weak = cx.entity().downgrade();
        let ctx_card = crate::context_ui::meeting_card(self.controller(), &m.dir, &m.meta, self.context_open, move |cx| {
            if let Some(w) = weak.upgrade() {
                w.update(cx, |w, cx| {
                    w.context_open = !w.context_open;
                    cx.notify();
                });
            }
        }, cx);
        // Context sits with the tools, above the notes it informed.
        let tools = match (tools, ctx_card) {
            (Some(t), Some(c)) => Some(div().flex().flex_col().child(t).child(c).into_any_element()),
            (t, c) => t.or(c),
        };
        let extras = self.recipe_results(m, cx);
        crate::meeting_view::render(
            m,
            self.show_transcript,
            self.meeting_match.map(|i| (i, self.match_anchor.clone())),
            speakers,
            cx.listener(|this, _, _, cx| {
                this.show_transcript = !this.show_transcript;
                cx.notify();
            }),
            {
                let c = self.controller().read(cx);
                let default_ollama = stenly_core::ollama::api_base(&c.config.llm.endpoint) == "http://localhost:11434";
                if !c.config.llm.enabled { "Turn on summaries" } else if !c.config.llm.uses_api() && default_ollama && !c.ollama.running { "Set up summaries" } else { "Summarize now" }
            },
            cx.listener(|this, _, _, cx| {
                let setup_page = {
                    let c = this.controller().read(cx);
                    // `ollama.running` is deliberately only used for the
                    // default Ollama URL. A custom local OpenAI-compatible
                    // endpoint (llama.cpp, LM Studio, vLLM) is allowed through
                    // to the backend-neutral pipeline probe.
                    let default_ollama = stenly_core::ollama::api_base(&c.config.llm.endpoint) == "http://localhost:11434";
                    if !c.config.llm.enabled { Some(Page::Preferences) } else if !c.config.llm.uses_api() && default_ollama && !c.ollama.running { Some(Page::Summaries) } else { None }
                };
                if let Some(page) = setup_page {
                    this.go(page, cx);
                    return;
                }
                let Some(m) = this.meeting.as_ref() else { return };
                let dir = m.dir.clone();
                this.controller().update(cx, |c, cx| c.reprocess(dir, cx));
                this.flash("Summarizing notes…", cx);
            }),
            tools,
            extras,
        )
    }

    fn meeting_tools(&self, m: &crate::meeting_view::MeetingDetail, cx: &mut Context<Self>) -> Option<AnyElement> {
        let words = m.transcript.as_ref().map(|t| t.word_count()).unwrap_or(0);
        if words == 0 || !matches!(m.meta.state, stenly_core::session::SessionState::Done) {
            return None;
        }
        let cfg = self.controller().read(cx).config.clone();
        let tlib = Library::templates();
        let templates = tlib.all();
        let chosen = recipe::chosen_template(&m.dir);
        let auto = stenly_core::recipe::auto_template(&templates, &cfg.templates.default, &recipe::MeetingInfo::from_meta(&m.meta));
        let auto_id = if cfg.templates.auto { auto } else { cfg.templates.default.clone() };
        let name_of = |id: &str| templates.iter().find(|t| t.id == id).map(|t| t.name.clone()).unwrap_or_else(|| "General".into());
        // What the note on screen was written with (old notes: General).
        let shown = m.summary.as_ref().and_then(|s| s.template.clone()).unwrap_or_else(|| "general".into());
        let regenerating = self.library.regenerating.as_ref() == Some(&m.dir);
        let label: SharedString = if regenerating { format!("Rewriting as {}…", name_of(chosen.as_deref().unwrap_or(&auto_id))).into() } else { name_of(&shown).into() };

        let mut items = vec![format!("Automatic ({})", name_of(&auto_id)), "-".to_string()];
        items.extend(templates.iter().map(|t| t.name.clone()));
        items.push("-".into());
        items.push("Regenerate Notes".into());
        items.push("Edit Templates…".into());
        let selected = match &chosen {
            None => Some(0),
            Some(c) => templates.iter().position(|t| &t.id == c).map(|i| i + 2),
        };
        let ids: Vec<String> = templates.iter().map(|t| t.id.clone()).collect();
        let n = ids.len();
        let weak = cx.entity().downgrade();
        let dir = m.dir.clone();
        let template_btn = menu_button("template-pick", Some(icons::FILE_TEXT), label, items, selected, move |i, cx| {
            let Some(this) = weak.upgrade() else { return };
            let dir = dir.clone();
            this.update(cx, |this, cx| {
                let pick: Option<Option<String>> = match i {
                    0 => Some(None),
                    i if (2..2 + n).contains(&i) => Some(Some(ids[i - 2].clone())),
                    i if i == n + 3 => Some(recipe::chosen_template(&dir)), // regenerate as is
                    i if i == n + 4 => {
                        this.go(Page::Templates, cx);
                        None
                    }
                    _ => None,
                };
                if let Some(t) = pick {
                    this.regenerate(dir, t, cx);
                }
            });
        });

        let rlib = Library::recipes();
        let recipes = rlib.all();
        let mut ritems: Vec<String> = recipes.iter().map(|r| r.name.clone()).collect();
        ritems.push("-".into());
        ritems.push("Edit Recipes…".into());
        let rids: Vec<String> = recipes.iter().map(|r| r.id.clone()).collect();
        let weak = cx.entity().downgrade();
        let dir = m.dir.clone();
        let recipe_btn = menu_button("recipe-run", Some(icons::SPARKLES), "Run Recipe", ritems, None, move |i, cx| {
            let Some(this) = weak.upgrade() else { return };
            let dir = dir.clone();
            let rids = rids.clone();
            this.update(cx, |this, cx| {
                if let Some(id) = rids.get(i) {
                    this.run_recipe(dir, id.clone(), cx);
                } else if i == rids.len() + 1 {
                    this.go(Page::Recipes, cx);
                }
            });
        });
        Some(
            div()
                .pt(px(14.))
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().text_size(px(theme::BODY)).text_color(theme::muted()).child("Template"))
                .child(template_btn)
                .when(regenerating, |d| d.child(ui::spinner("regen-spin", 12., theme::muted())))
                .child(div().flex_1())
                .child(recipe_btn)
                .into_any_element(),
        )
    }

    /// Rewrite the note with `template` (None = automatic) from the existing transcript.
    pub(crate) fn regenerate(&mut self, dir: PathBuf, template: Option<String>, cx: &mut Context<Self>) {
        if let Err(e) = recipe::set_chosen_template(&dir, template.as_deref()) {
            self.controller().update(cx, |c, cx| {
                c.last_error = Some(format!("Couldn't save the template choice: {e:#}"));
                cx.notify();
            });
            return;
        }
        self.library.regenerating = Some(dir.clone());
        self.controller().update(cx, |c, cx| c.reprocess(dir, cx));
        cx.notify();
    }

    pub(crate) fn run_recipe(&mut self, dir: PathBuf, id: String, cx: &mut Context<Self>) {
        let key = (dir.clone(), id.clone());
        if self.library.running.contains(&key) {
            return;
        }
        let Ok(entry) = Library::recipes().find(&id) else { return };
        self.library.errors.retain(|(d, r, _)| !(d == &dir && r == &id));
        self.library.running.push(key.clone());
        self.library.toggled.remove(&key);
        let cfg = self.controller().read(cx).config.clone();
        let work = cx.background_spawn(async move {
            let target = cfg.llm.target(stenly_core::secrets::llm_api_key);
            let r = recipe::run(&cfg, &target, &entry, &dir)?;
            recipe::push_result(&dir, r)?;
            anyhow::Ok(())
        });
        cx.spawn(async move |this, cx| {
            let res = work.await;
            let _ = this.update(cx, |this, cx| {
                this.library.running.retain(|k| k != &key);
                if let Err(e) = res {
                    tracing::warn!(error = %format!("{e:#}"), recipe = %key.1, "recipe failed");
                    this.library.errors.push((key.0.clone(), key.1.clone(), format!("{e:#}")));
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn recipe_results(&self, m: &crate::meeting_view::MeetingDetail, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let st = &self.library;
        let running: Vec<&String> = st.running.iter().filter(|(d, _)| d == &m.dir).map(|(_, r)| r).collect();
        let errors: Vec<(&String, &String)> = st.errors.iter().filter(|(d, _, _)| d == &m.dir).map(|(_, r, e)| (r, e)).collect();
        if m.results.is_empty() && running.is_empty() && errors.is_empty() {
            return vec![];
        }
        let rlib = Library::recipes();
        let name_of = |id: &str| rlib.get(id).map(|e| e.name).unwrap_or_else(|| id.to_string());
        let has_note = m.meta.note_path.as_ref().is_some_and(|p| p.exists());
        let mut cards: Vec<AnyElement> = Vec::new();
        // newest first
        let newest = m.results.len().saturating_sub(1);
        for (i, r) in m.results.iter().enumerate().rev() {
            let busy = running.contains(&&r.recipe);
            let key = (m.dir.clone(), r.recipe.clone());
            let open = ((i == newest) != st.toggled.contains(&key)) || busy;
            cards.push(self.result_card(i, r, open, busy, has_note, &m.dir, cx));
        }
        for id in running.iter().filter(|id| !m.results.iter().any(|r| &&r.recipe == *id)) {
            cards.insert(
                0,
                card_shell()
                    .child(div().flex().items_center().gap(px(8.)).child(ui::spinner(SharedString::from(format!("rr-{id}")), 12., theme::muted())).child(div().text_size(px(theme::BODY)).text_color(theme::muted()).child(format!("Running “{}”…", name_of(id)))))
                    .into_any_element(),
            );
        }
        for (j, (id, e)) in errors.iter().enumerate() {
            let weak = cx.entity().downgrade();
            let (dir, id2, dir2, id3) = (m.dir.clone(), (*id).clone(), m.dir.clone(), (*id).clone());
            cards.insert(
                0,
                card_shell()
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .gap(px(8.))
                            .child(div().pt(px(2.)).child(icon(icons::ALERT, 12., theme::amber())))
                            .child(div().flex_1().min_w_0().flex().flex_col().gap(px(2.)).child(div().text_size(px(theme::BODY)).font_weight(FontWeight::MEDIUM).child(format!("“{}” didn't run", name_of(id)))).child(div().text_size(px(theme::SUBHEAD)).line_height(px(15.)).text_color(theme::muted()).child(e.chars().take(300).collect::<String>())))
                            .child(ui::button(("rr-retry", j), "Try Again", Btn::Ghost).on_click(cx.listener(move |this, _, _, cx| this.run_recipe(dir.clone(), id2.clone(), cx))))
                            .child(ui::icon_button(("rr-err-x", j), icons::X, 22., 12., "Dismiss").on_click(move |_, _, cx| {
                                if let Some(this) = weak.upgrade() {
                                    this.update(cx, |this, cx| {
                                        this.library.errors.retain(|(d, r, _)| !(d == &dir2 && r == &id3));
                                        cx.notify();
                                    });
                                }
                            })),
                    )
                    .into_any_element(),
            );
        }
        vec![
            div()
                .pt(px(26.))
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(div().text_size(px(theme::TITLE3)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child("Recipes"))
                .children(cards)
                .into_any_element(),
        ]
    }

    #[allow(clippy::too_many_arguments)]
    fn result_card(&self, i: usize, r: &RecipeResult, open: bool, busy: bool, has_note: bool, dir: &std::path::Path, cx: &mut Context<Self>) -> AnyElement {
        let key = (dir.to_path_buf(), r.recipe.clone());
        let text = r.text.clone();
        let heading = Library::recipes().get(&r.recipe).map(|e| recipe::default_heading(&e)).unwrap_or_else(|| r.name.clone());
        let meta = format!("{} · {}", ago(r.created), r.model);
        let toggle_key = key.clone();
        let header = div()
            .id(("rr-head", i))
            .flex()
            .items_center()
            .gap(px(6.))
            .min_h(px(24.))
            .child(icon(if open { icons::CHEVRON_DOWN } else { icons::CHEVRON_RIGHT }, 10., theme::muted()))
            .child(div().text_size(px(theme::BODY)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child(r.name.clone()))
            .child(div().flex_1().min_w_0().truncate().text_size(px(theme::SUBHEAD)).text_color(theme::faint()).child(meta))
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.library.toggled.remove(&toggle_key) {
                    this.library.toggled.insert(toggle_key.clone());
                }
                cx.notify();
            }));
        let actions = {
            let (d1, d2, d3) = (dir.to_path_buf(), dir.to_path_buf(), dir.to_path_buf());
            let (id1, id2, id3) = (r.recipe.clone(), r.recipe.clone(), r.recipe.clone());
            let heading2 = heading.clone();
            // One inline action (Add to Note while it can be added, else Copy);
            // everything else in "…".
            let add_inline = has_note && r.saved_heading.is_none();
            let copy_text = text.clone();
            let mut entries = Vec::new();
            let mut acts: Vec<&'static str> = Vec::new();
            if add_inline {
                entries.push(MenuEntry { symbol: Some("doc.on.doc"), ..MenuEntry::item("Copy") });
                acts.push("copy");
            }
            entries.push(MenuEntry { symbol: Some("arrow.clockwise"), ..MenuEntry::item("Run Again") });
            acts.push("rerun");
            entries.push(MenuEntry::separator());
            acts.push("");
            entries.push(MenuEntry { symbol: Some("trash"), ..MenuEntry::item("Remove") });
            acts.push("remove");
            let weak = cx.entity().downgrade();
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .flex_shrink_0()
                .when(busy, |d| d.child(ui::spinner(("rr-busy", i), 12., theme::muted())))
                .when(has_note && r.saved_heading.is_some(), |d| d.child(ui::chip("In Note", Some(icons::CHECK), theme::green())))
                .child(if add_inline {
                    ui::button(("rr-save", i), "Add to Note", Btn::Ghost).on_click(cx.listener(move |this, _, _, cx| {
                        match recipe::save_to_note(&d1, &id1, &heading2) {
                            Ok(_) => this.flash("Added to note", cx),
                            Err(e) => this.controller().update(cx, |c, cx| {
                                c.last_error = Some(format!("Couldn't add it to the note: {e:#}"));
                                cx.notify();
                            }),
                        }
                    }))
                } else {
                    ui::button(("rr-copy", i), "Copy", Btn::Ghost).on_click(cx.listener(move |this, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                        this.flash("Copied", cx);
                    }))
                })
                .child(ui::more_button(("rr-more", i), entries, move |k, _, cx| {
                    let Some(this) = weak.upgrade() else { return };
                    this.update(cx, |this, cx| match acts[k] {
                        "copy" => {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                            this.flash("Copied", cx);
                        }
                        "rerun" => this.run_recipe(d2.clone(), id2.clone(), cx),
                        "remove" => {
                            if let Err(e) = recipe::remove_result(&d3, &id3) {
                                tracing::warn!(error = %format!("{e:#}"), "removing recipe result");
                            }
                            this.library.toggled.remove(&(d3.clone(), id3.clone()));
                            cx.notify();
                        }
                        _ => {}
                    });
                }))
        };
        card_shell()
            .child(div().flex().items_center().gap(px(8.)).child(div().flex_1().min_w_0().child(header)).child(actions))
            .when(open, |d| d.child(div().pt(px(10.)).opacity(if busy { 0.5 } else { 1. }).child(markdown(&r.text))))
            .when(open && r.saved_heading.is_some(), |d| {
                d.child(div().pt(px(10.)).text_size(px(theme::SUBHEAD)).text_color(theme::faint()).child(format!("Saved in the note under “{}”.", r.saved_heading.clone().unwrap_or_default())))
            })
            .into_any_element()
    }
}

fn card_shell() -> gpui::Div {
    div().w_full().px(px(14.)).py(px(10.)).rounded(px(10.)).bg(theme::card()).flex().flex_col()
}

// ---------------------------------------------------------------- Settings pages

const LANGUAGES: &[&str] = &["", "English", "Japanese", "Spanish", "French", "German", "Portuguese", "Italian", "Dutch", "Chinese (Simplified)", "Korean"];

impl MainWindow {
    pub(crate) fn library_page(&mut self, kind: Kind, cx: &mut Context<Self>) -> AnyElement {
        // Dev: STENLY_EDIT=<id> (with STENLY_PAGE=templates|recipes) opens the editor; "new" for a blank one.
        static DEV_EDIT: std::sync::Once = std::sync::Once::new();
        DEV_EDIT.call_once(|| {
            if let Ok(id) = std::env::var("STENLY_EDIT") {
                let ed = self.build_editor(kind, (id != "new").then_some(id), cx);
                self.library.editor = Some(ed);
            }
        });
        if self.library.editor.as_ref().is_some_and(|e| e.kind == kind) {
            return self.editor_page(cx);
        }
        let l = lib(kind);
        let all = l.all_with_hidden();
        let visible: Vec<&Entry> = all.iter().filter(|e| !e.hidden).collect();
        let hidden: Vec<&Entry> = all.iter().filter(|e| e.hidden).collect();
        let cfg = self.controller().read(cx).config.clone();

        let rows: Vec<AnyElement> = visible
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let id = e.id.clone();
                let badge = match (e.origin, kind == Kind::Template && e.id == cfg.templates.default) {
                    (_, true) => Some("Default"),
                    (Origin::Modified, _) => Some("Edited"),
                    (Origin::Custom, _) => Some("Custom"),
                    _ => None,
                };
                let mut items = vec!["Edit…".to_string(), "Duplicate".to_string()];
                if kind == Kind::Template && e.id != cfg.templates.default {
                    items.push("Use by Default".into());
                }
                items.push("-".into());
                if e.origin == Origin::Modified {
                    items.push("Restore Original".into());
                }
                items.push(if e.is_builtin() { "Hide".into() } else { "Delete".into() });
                let weak = cx.entity().downgrade();
                let menu_items = items.clone();
                let id_menu = id.clone();
                let ctrl = self.controller().clone();
                div()
                    .id(("lib-row", i))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .min_h(px(48.))
                    .py(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .flex()
                                    .items_baseline()
                                    .gap(px(6.))
                                    .child(div().text_size(px(theme::BODY)).text_color(theme::text()).child(e.name.clone()))
                                    .when_some(badge, |d, b| d.child(div().text_size(px(theme::SUBHEAD)).text_color(theme::faint()).child(b))),
                            )
                            .child(div().text_size(px(theme::SUBHEAD)).line_height(px(15.)).text_color(theme::muted()).truncate().child(e.description.clone())),
                    )
                    .child(ui::button(("lib-edit", i), "Edit", Btn::Ghost).on_click(cx.listener(move |this, _, window, cx| this.open_editor(kind, Some(id.clone()), window, cx))))
                    .child(ui::icon_button(("lib-menu", i), icons::ELLIPSIS, 22., 12., "More").on_click(move |ev, window, cx| {
                        let pos = ev.mouse_position().unwrap_or_else(|| window.mouse_position());
                        let view = ui::native_view(window);
                        let (items, weak, id, ctrl) = (menu_items.clone(), weak.clone(), id_menu.clone(), ctrl.clone());
                        window
                            .spawn(cx, async move |cx| {
                                let labels: Vec<&str> = items.iter().map(String::as_str).collect();
                                let Some(i) = ui::menu_in_view(view, &labels, None, pos) else { return };
                                let picked = items[i].clone();
                                let _ = cx.update(|window, cx| {
                                    let Some(this) = weak.upgrade() else { return };
                                    this.update(cx, |this, cx| this.library_action(kind, &id, &picked, &ctrl, window, cx));
                                });
                            })
                            .detach();
                    }))
                    .into_any_element()
            })
            .collect();

        let folder = l.dir.clone();
        let new_btn = ui::button_lead_icon(SharedString::from(format!("lib-new-{kind:?}")), format!("New {}", noun(kind)), Btn::Ghost, icons::PLUS)
            .on_click(cx.listener(move |this, _, window, cx| this.open_editor(kind, None, window, cx)));
        let folder_btn = ui::button(SharedString::from(format!("lib-folder-{kind:?}")), "Show in Finder", Btn::Ghost).on_click(move |_, _, cx| {
            let _ = std::fs::create_dir_all(&folder);
            cx.open_with_system(&folder)
        });

        let mut page = div().flex().flex_col();
        if kind == Kind::Template {
            let ctrl = self.controller().clone();
            let auto_on = cfg.templates.auto;
            let auto_row = ui::row(
                None,
                "Choose automatically",
                Some("From the meeting's title and guests, e.g. “1:1”, “Stand-up” or “Demo”".into()),
                ui::toggle("tpl-auto", auto_on, move |_, _, cx| ctrl.update(cx, |c, cx| c.set("templates.auto", !auto_on, cx))),
            )
            .into_any_element();
            let ids: Vec<String> = visible.iter().map(|e| e.id.clone()).collect();
            let names: Vec<String> = visible.iter().map(|e| e.name.clone()).collect();
            let cur = ids.iter().position(|i| i == &cfg.templates.default);
            let ctrl = self.controller().clone();
            let default_row = ui::row(
                None,
                if auto_on { "Otherwise use" } else { "Use" },
                Some(if auto_on { "When nothing about the meeting suggests a template".into() } else { "For every meeting".into() }),
                menu_button("tpl-default", None, cur.map(|i| names[i].clone()).unwrap_or_else(|| "General".into()), names.clone(), cur, move |i, cx| {
                    let id = ids[i].clone();
                    ctrl.update(cx, |c, cx| c.set("templates.default", id.as_str(), cx))
                }),
            )
            .into_any_element();
            let ctrl = self.controller().clone();
            let lang = cfg.llm.language.clone();
            let mut langs: Vec<String> = LANGUAGES.iter().map(|l| if l.is_empty() { "Same as the meeting".to_string() } else { l.to_string() }).collect();
            let mut values: Vec<String> = LANGUAGES.iter().map(|l| l.to_string()).collect();
            if !lang.is_empty() && !values.iter().any(|v| v.eq_ignore_ascii_case(&lang)) {
                langs.push(lang.clone());
                values.push(lang.clone());
            }
            let cur = values.iter().position(|v| v.eq_ignore_ascii_case(&lang));
            let lang_row = ui::row(
                None,
                "Language",
                Some("Notes and recipe results are written in this language".into()),
                menu_button("tpl-lang", None, cur.map(|i| langs[i].clone()).unwrap_or_default(), langs, cur, move |i, cx| {
                    let v = values[i].clone();
                    ctrl.update(cx, |c, cx| c.set("llm.language", v.as_str(), cx))
                }),
            )
            .into_any_element();
            page = page.child(ui::caption("Notes")).child(ui::card(vec![auto_row, default_row, lang_row]));
        }
        page = page
            .child(ui::caption(match kind {
                Kind::Template => "Templates",
                Kind::Recipe => "Recipes",
            }))
            .child(ui::card(rows))
            .child(div().pt(px(10.)).flex().gap(px(8.)).child(new_btn).child(div().flex_1()).child(folder_btn));
        if !hidden.is_empty() {
            let rows: Vec<AnyElement> = hidden
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    let id = e.id.clone();
                    ui::row(
                        None,
                        e.name.clone(),
                        Some(e.description.clone().into()),
                        ui::button(("lib-restore", i), "Show", Btn::Ghost).on_click(cx.listener(move |_, _, _, cx| {
                            let _ = lib(kind).reset(&id);
                            cx.notify();
                        })),
                    )
                    .into_any_element()
                })
                .collect();
            page = page.child(ui::caption("Hidden")).child(ui::card(rows));
        }
        page.into_any_element()
    }

    fn library_action(&mut self, kind: Kind, id: &str, action: &str, ctrl: &Entity<crate::controller::Controller>, window: &mut Window, cx: &mut Context<Self>) {
        let l = lib(kind);
        let res: anyhow::Result<()> = match action {
            "Edit…" => {
                self.open_editor(kind, Some(id.to_string()), window, cx);
                Ok(())
            }
            "Duplicate" => l.duplicate(id).map(|e| self.open_editor(kind, Some(e.id), window, cx)),
            "Use by Default" => {
                ctrl.update(cx, |c, cx| c.set("templates.default", id, cx));
                Ok(())
            }
            "Restore Original" => l.reset(id),
            "Hide" | "Delete" => {
                if kind == Kind::Template && ctrl.read(cx).config.templates.default == id {
                    ctrl.update(cx, |c, cx| c.set("templates.default", "general", cx));
                }
                l.delete(id)
            }
            _ => Ok(()),
        };
        if let Err(e) = res {
            ctrl.update(cx, |c, cx| {
                c.last_error = Some(format!("{e:#}"));
                cx.notify();
            });
        }
        cx.notify();
    }

    pub(crate) fn open_editor(&mut self, kind: Kind, id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let is_new = id.is_none();
        let ed = self.build_editor(kind, id, cx);
        let focus = if is_new { ed.name.clone() } else { ed.body.clone() };
        self.library.editor = Some(ed);
        self.page = match kind {
            Kind::Template => Page::Templates,
            Kind::Recipe => Page::Recipes,
        };
        self.meeting = None;
        focus.update(cx, |t, cx| window.focus(&gpui::Focusable::focus_handle(t, cx), cx));
        cx.notify();
    }

    fn build_editor(&mut self, kind: Kind, id: Option<String>, cx: &mut Context<Self>) -> Editor {
        let l = lib(kind);
        let e = id.as_deref().and_then(|i| l.get(i)).unwrap_or_else(|| {
            let mut e = Entry::new("", "");
            e.body = match kind {
                Kind::Template => "Describe the meeting and what you want from the notes.\n\n## First section\nWhat belongs here.\n\n## Second section\nWhat belongs here.".into(),
                Kind::Recipe => "Explain the job, then the rules and the format you want back.".into(),
            };
            e
        });
        let field = |cx: &mut Context<Self>, ph: &str, text: &str| {
            let (ph, text) = (ph.to_string(), text.to_string());
            cx.new(move |cx| {
                let mut t = TextInput::new(cx, ph);
                t.set_text(text, cx);
                t.move_to_start(cx);
                t
            })
        };
        let name = field(cx, &format!("{} name", noun(kind)), &e.name);
        let description = field(cx, "One line about when to use it", &e.description);
        let extra = match kind {
            Kind::Template => field(cx, "e.g. 1:1, one on one", e.field("match").unwrap_or("")),
            Kind::Recipe => field(cx, &e.name, e.field("heading").unwrap_or("")),
        };
        let body_text = e.body.clone();
        let body = cx.new(move |cx| {
            let mut t = TextInput::new(cx, "Instructions").editor(22);
            t.set_text(body_text, cx);
            t.move_to_start(cx);
            t
        });
        Editor { kind, id: id.filter(|i| !i.is_empty()), origin: e.origin, name, description, extra, body, error: None }
    }

    fn save_editor(&mut self, cx: &mut Context<Self>) {
        let Some(ed) = &mut self.library.editor else { return };
        let l = lib(ed.kind);
        let name = ed.name.read(cx).text().trim().to_string();
        if name.is_empty() {
            ed.error = Some("Give it a name.".into());
            cx.notify();
            return;
        }
        let body = ed.body.read(cx).text().trim().to_string();
        if body.is_empty() {
            ed.error = Some("Write some instructions.".into());
            cx.notify();
            return;
        }
        let mut e = match &ed.id {
            Some(id) => l.get(id).unwrap_or_else(|| Entry::new(id.clone(), name.clone())),
            None => Entry::new(l.new_id(&name), name.clone()),
        };
        e.name = name;
        e.description = ed.description.read(cx).text().trim().to_string();
        e.body = body;
        e.hidden = false;
        let extra = ed.extra.read(cx).text().trim().to_string();
        e.set_field(if ed.kind == Kind::Template { "match" } else { "heading" }, &extra);
        if ed.kind == Kind::Recipe && recipe::is_reserved_heading(&extra) {
            ed.error = Some(format!("“{extra}” is one of the note's own sections; pick another heading."));
            cx.notify();
            return;
        }
        // An unchanged built-in stays a built-in (no override file).
        let unchanged = l.builtin(&e.id).is_some_and(|b| b.name == e.name && b.description == e.description && b.body == e.body && b.fields.iter().filter(|(_, v)| !v.is_empty()).eq(e.fields.iter().filter(|(_, v)| !v.is_empty())));
        let res = if unchanged { l.reset(&e.id) } else { l.save(&e).map(|_| ()) };
        match res {
            Ok(()) => {
                self.library.editor = None;
                self.flash("Saved", cx);
            }
            Err(err) => {
                if let Some(ed) = &mut self.library.editor {
                    ed.error = Some(format!("{err:#}"));
                }
            }
        }
        cx.notify();
    }

    /// Toolbar title while a template/recipe editor is open on `kind`'s page.
    pub(crate) fn library_editor_title(&self, kind: Kind) -> Option<SharedString> {
        let ed = self.library.editor.as_ref().filter(|e| e.kind == kind)?;
        Some(match &ed.id {
            Some(_) => format!("Edit {}", noun(kind)).into(),
            None => format!("New {}", noun(kind)).into(),
        })
    }

    pub(crate) fn close_library_editor(&mut self, cx: &mut Context<Self>) {
        self.library.editor = None;
        cx.notify();
    }

    fn editor_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(ed) = &self.library.editor else { return div().into_any_element() };
        let kind = ed.kind;
        let field_row = |label: &'static str, input: Entity<TextInput>| {
            div().flex().items_center().gap(px(12.)).min_h(px(44.)).child(div().w(px(96.)).flex_shrink_0().text_size(px(theme::BODY)).child(label)).child(ui::text_field(input)).into_any_element()
        };
        let extra_label = match kind {
            Kind::Template => "Use for titles",
            Kind::Recipe => "Note heading",
        };
        let extra_help = match kind {
            Kind::Template => "Comma-separated words. A meeting whose title or invite contains one of them gets this template.",
            Kind::Recipe => "“Add to Note” puts the result under this heading in the meeting's note.",
        };
        let help = match kind {
            Kind::Template => "Start with what the meeting is and what you want from it, then add a “## Heading” per section with a line on what goes there. Sections nothing fits are left out.",
            Kind::Recipe => "Write it like a brief for a sharp assistant: the job, the rules, and the format of the answer. The meeting's notes and transcript are added for you.",
        };
        let is_builtin = ed.id.as_deref().is_some_and(|i| lib(kind).builtin(i).is_some());
        let can_delete = ed.id.is_some() && !is_builtin;
        let id = ed.id.clone();
        let mut footer = div().pt(px(12.)).flex().items_center().gap(px(8.));
        if can_delete {
            let id = id.clone().unwrap_or_default();
            footer = footer.child(ui::button("ed-delete", "Delete", Btn::Danger).on_click(cx.listener(move |this, _, _, cx| {
                let _ = lib(kind).delete(&id);
                this.library.editor = None;
                cx.notify();
            })));
        }
        if is_builtin && ed.origin == Origin::Modified {
            let id = id.clone().unwrap_or_default();
            footer = footer.child(ui::button("ed-restore", "Restore Original", Btn::Ghost).on_click(cx.listener(move |this, _, window, cx| {
                let _ = lib(kind).reset(&id);
                this.open_editor(kind, Some(id.clone()), window, cx);
            })));
        }
        footer = footer
            .child(div().flex_1())
            .when_some(ed.error.clone(), |d, e| d.child(div().flex().items_center().gap(px(4.)).text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child(icon(icons::ALERT, 11., theme::amber())).child(e)))
            .child(ui::button("ed-cancel", "Cancel", Btn::Ghost).on_click(cx.listener(|this, _, _, cx| {
                this.library.editor = None;
                cx.notify();
            })))
            .child(ui::button("ed-save", "Save", Btn::Primary).on_click(cx.listener(|this, _, _, cx| this.save_editor(cx))));
        div()
            .flex()
            .flex_col()
            .child(ui::caption("Details"))
            .child(ui::card(vec![
                field_row("Name", ed.name.clone()),
                field_row("Description", ed.description.clone()),
                div()
                    .flex()
                    .flex_col()
                    .child(field_row(extra_label, ed.extra.clone()))
                    .child(div().pl(px(108.)).pb(px(10.)).text_size(px(theme::SUBHEAD)).line_height(px(15.)).text_color(theme::muted()).child(extra_help))
                    .into_any_element(),
            ]))
            .child(ui::caption("Instructions"))
            .child(
                div()
                    .rounded(px(12.))
                    .bg(theme::card())
                    .p(px(4.))
                    .child(
                        div()
                            .px(px(12.))
                            .py(px(10.))
                            .flex()
                            .flex_col()
                            .gap(px(10.))
                            .child(div().text_size(px(theme::SUBHEAD)).line_height(px(15.)).text_color(theme::muted()).child(help))
                            .child(
                                div()
                                    .min_h(px(320.))
                                    .px(px(9.))
                                    .py(px(7.))
                                    .rounded(px(6.))
                                    .bg(theme::inset())
                                    .border_1()
                                    .border_color(theme::divider())
                                    .text_size(px(theme::BODY))
                                    .line_height(px(19.))
                                    .font_family("Menlo")
                                    .child(ed.body.clone()),
                            ),
                    ),
            )
            .child(footer)
            .into_any_element()
    }
}
