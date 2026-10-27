//! Settings → Dictation, Microphones, and the Dictations history list.
//! Same grammar as the other settings pages (grouped cards, 48 pt rows).

use crate::icons::{self, icon};
use crate::main_window::MainWindow;
use crate::theme;
use crate::ui::{self, Btn};
use chrono::Local;
use gpui::{AnyElement, Context, FontWeight, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px};
use stenly_core::config::{ShortcutMode, StreamMode};
use stenly_core::platform::{Permission, PermissionState};

/// Three 19pt lines: cap the preview, never the metadata or disclosure control.
const DICTATION_PREVIEW_HEIGHT: f32 = 3. * 19.;

impl MainWindow {
    pub(crate) fn refresh_mics(&mut self, cx: &mut Context<Self>) {
        self.mics = self.controller().read(cx).input_devices();
        cx.notify();
    }

    // ------------------------------------------------------------- Dictation
    pub(crate) fn dictation_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let ctrl = self.controller().clone();
        let c = ctrl.read(cx);
        let cfg = c.config.clone();
        let d = cfg.dictation.clone();
        let ax = c.permission(Permission::Accessibility);
        let dict = crate::dictation_entity();
        let hotkey_err = dict.as_ref().and_then(|e| e.read(cx).hotkeys_error.clone());
        let tog = |id: &'static str, key: &'static str, on: bool| {
            let ctrl = ctrl.clone();
            ui::toggle(id, on, move |_, _, cx| ctrl.update(cx, |c, cx| c.set(key, !on, cx))).into_any_element()
        };
        let mode_popup = |id: &'static str, key: &'static str, mode: ShortcutMode| {
            let ctrl = ctrl.clone();
            ui::popup(id, &[(ShortcutMode::Hybrid, "Tap or hold"), (ShortcutMode::Toggle, "Toggle"), (ShortcutMode::PushToTalk, "Push to talk")], mode, move |m, cx| {
                let v = match m {
                    ShortcutMode::Hybrid => "hybrid",
                    ShortcutMode::Toggle => "toggle",
                    ShortcutMode::PushToTalk => "push_to_talk",
                };
                ctrl.update(cx, |c, cx| c.set(key, v, cx))
            })
            .into_any_element()
        };
        let mode_help = match d.mode {
            ShortcutMode::Hybrid => "Tap to start hands-free (tap again to stop). Hold to talk, release to stop.",
            ShortcutMode::Toggle => "Press to start, press again to stop.",
            ShortcutMode::PushToTalk => "Hold while you talk; release to stop.",
        };

        // Permission banner: without Accessibility nothing below works.
        let needs_ax = ax != PermissionState::Granted && ax != PermissionState::NotApplicable;
        let banner: Option<AnyElement> = if needs_ax || hotkey_err.is_some() {
            let ctrl2 = ctrl.clone();
            let msg = if needs_ax {
                "Stenly needs Accessibility access to hear your shortcut and type the text for you."
            } else {
                "Shortcuts aren't active."
            };
            let detail = hotkey_err.clone().filter(|_| !needs_ax).unwrap_or_default();
            Some(
                div()
                    .mt(px(16.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(14.))
                    .py(px(12.))
                    .rounded(px(10.))
                    .bg(theme::card())
                    .child(icon(icons::ALERT, 15., theme::amber()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(div().text_size(px(theme::BODY)).child(msg))
                            .when(!detail.is_empty(), |x| x.child(div().text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child(detail))),
                    )
                    .when(needs_ax, |x| {
                        x.child(ui::button("ax-allow", "Allow…", Btn::Accent).on_click(move |_, _, cx| ctrl2.update(cx, |c, cx| c.request_permission(Permission::Accessibility, cx))))
                    })
                    .into_any_element(),
            )
        } else {
            None
        };

        let shortcut_row = |title: &'static str, desc: Option<&'static str>, field: gpui::Entity<crate::shortcut_field::ShortcutField>, mode: Option<AnyElement>| {
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .min_h(px(48.))
                .py(px(9.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(div().text_size(px(theme::BODY)).child(title))
                        .when_some(desc, |x, s| x.child(div().text_size(px(theme::SUBHEAD)).line_height(px(15.)).text_color(theme::muted()).child(s))),
                )
                .when_some(mode, |x, m| x.child(m))
                .child(field)
                .into_any_element()
        };

        let restore_secs = d.restore_clipboard_secs;
        let restore_btn = {
            let ctrl = ctrl.clone();
            let cur = (restore_secs * 10.).round() as i64;
            ui::popup("restore-delay", &[(10i64, "After 1 s"), (15, "After 1.5 s"), (20, "After 2 s"), (30, "After 3 s"), (50, "After 5 s")], cur, move |v, cx| {
                ctrl.update(cx, |c, cx| c.set("dictation.restore_clipboard_secs", v as f64 / 10.0, cx))
            })
            .into_any_element()
        };
        let ai_prompt = {
            let ctrl = ctrl.clone();
            let cur = if d.ai_prompt == "assistant" { 1 } else { 0 };
            ui::popup("ai-prompt", &[(0u8, "Clean up"), (1u8, "Assistant")], cur, move |v, cx| {
                ctrl.update(cx, |c, cx| c.set("dictation.ai_prompt", if v == 1 { "assistant" } else { "clean" }, cx))
            })
            .into_any_element()
        };
        let llm_label = if cfg.llm.uses_api() { format!("{} (API)", cfg.llm.api.model) } else { format!("{} on this Mac", cfg.llm.model) };

        // Replacements: "heard as → write as" chips + an add row.
        let reps: Vec<AnyElement> = d
            .replacements
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let ctrl = ctrl.clone();
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .min_h(px(40.))
                    .child(div().flex_1().min_w_0().truncate().text_size(px(theme::BODY)).text_color(theme::muted()).child(r.from.clone()))
                    .child(icon(icons::CHEVRON_RIGHT, 10., theme::faint()))
                    .child(div().flex_1().min_w_0().truncate().text_size(px(theme::BODY)).font_weight(FontWeight::MEDIUM).child(r.to.clone()))
                    .child(
                        div()
                            .id(("rep-x", i))
                            .size(px(22.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .hover(|s| s.bg(theme::hover()))
                            .child(icon(icons::X, 9., theme::muted()))
                            .on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.remove_replacement(i, cx))),
                    )
                    .into_any_element()
            })
            .chain(std::iter::once(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .min_h(px(48.))
                    .child(ui::text_field(self.rep_from.clone()))
                    .child(icon(icons::CHEVRON_RIGHT, 10., theme::faint()))
                    .child(ui::text_field(self.rep_to.clone()))
                    .child({
                        let (ctrl, from, to) = (ctrl.clone(), self.rep_from.clone(), self.rep_to.clone());
                        ui::button("rep-add", "Add", Btn::Ghost).on_click(move |_, _, cx| {
                            let f = from.read(cx).text().trim().to_string();
                            let t = to.read(cx).text().trim().to_string();
                            if f.is_empty() || t.is_empty() {
                                return;
                            }
                            ctrl.update(cx, |c, cx| c.add_replacement(&f, &t, cx));
                            from.update(cx, |x, cx| x.set_text("", cx));
                            to.update(cx, |x, cx| x.set_text("", cx));
                        })
                    })
                    .into_any_element(),
            ))
            .collect();

        div()
            .flex()
            .flex_col()
            .when_some(banner, |x, b| x.child(b))
            .child(ui::caption("Shortcut"))
            .child(ui::card(vec![
                ui::row(None, "Dictation", Some("Use the shortcut in any app".into()), tog("dict-on", "dictation.enabled", d.enabled)).into_any_element(),
                shortcut_row("Shortcut", Some(mode_help), self.sc_main.clone(), Some(mode_popup("mode1", "dictation.mode", d.mode))),
                shortcut_row("Second shortcut", Some("Optional, e.g. a mouse button mapped to a key"), self.sc_second.clone(), Some(mode_popup("mode2", "dictation.mode2", d.mode2))),
                shortcut_row("Paste last dictation", None, self.sc_paste.clone(), None),
                ui::row(None, "Cancel", Some("Press Esc while dictating to discard it".into()), crate::shortcut_field::cap("Esc")).into_any_element(),
            ]))
            .child(ui::caption("Speech model"))
            .child(ui::card(vec![ui::row(
                None,
                "Memory use",
                Some("Speech recognition runs in a separate process. Unloading saves memory, but the next short dictation may take longer to finish.".into()),
                {
                    let ctrl = ctrl.clone();
                    ui::popup("speech-memory", &[(0u64, "Always ready"), (300, "Unload after 5 min"), (600, "Unload after 10 min"), (1800, "Unload after 30 min")], d.model_idle_secs, move |secs, cx| {
                        ctrl.update(cx, |c, cx| c.set("dictation.model_idle_secs", secs as i64, cx))
                    }).into_any_element()
                },
            ).into_any_element()]))
            .child(ui::caption("Pasting"))
            .child(ui::card(vec![
                ui::row(None, "Restore clipboard", Some("Put back what you had copied after the text is pasted".into()), div().flex().items_center().gap(px(8.)).when(d.restore_clipboard, |x| x.child(restore_btn)).child(tog("restore", "dictation.restore_clipboard", d.restore_clipboard))).into_any_element(),
                ui::row(None, "Press Return after pasting", Some("Sends the message in chat apps".into()), tog("autosend", "dictation.auto_send", d.auto_send)).into_any_element(),
            ]))
            .child(ui::caption("Recording"))
            .child(ui::card(vec![
                ui::row(None, "Sounds", Some("A soft chime when dictation starts and stops".into()), tog("sounds", "dictation.sounds", d.sounds)).into_any_element(),
                ui::row(None, "Mute other audio", Some("Silence music and videos while you dictate".into()), tog("mute", "dictation.mute_system_audio", d.mute_system_audio)).into_any_element(),
                ui::row(
                    None,
                    "Transcribe while you talk",
                    Some(
                        match d.stream {
                            StreamMode::Pause => "Text is ready about a second after you stop, however long you talk",
                            StreamMode::Overlap => "Re-reads a few seconds around each cut; slower to finish on a busy Mac",
                            StreamMode::Off => "Transcribes after you stop; long dictations take longer",
                        }
                        .into(),
                    ),
                    {
                        let ctrl = ctrl.clone();
                        ui::popup("stream", &[(StreamMode::Pause, "On"), (StreamMode::Overlap, "Overlapping (experimental)"), (StreamMode::Off, "Off")], d.stream, move |m, cx| {
                            let v = match m {
                                StreamMode::Overlap => "overlap",
                                StreamMode::Pause => "pause",
                                StreamMode::Off => "off",
                            };
                            ctrl.update(cx, |c, cx| c.set("dictation.stream", v, cx))
                        })
                        .into_any_element()
                    },
                )
                .into_any_element(),
                ui::row(None, "Live preview", Some("Show the words above the recorder as you speak".into()), tog("live-preview", "dictation.live_preview", d.live_preview && d.stream != StreamMode::Off)).into_any_element(),
            ]))
            .child(ui::caption("Text"))
            .child(ui::card(vec![
                ui::row(None, "Remove filler words", Some("Drop “um”, “uh” and [noise] tags".into()), tog("fillers", "dictation.remove_fillers", d.remove_fillers)).into_any_element(),
                ui::row(None, "Polish with AI", Some(SharedString::from(format!("Fix grammar and format lists with {llm_label} (Settings → Summaries). Adds about a second."))), div().flex().items_center().gap(px(8.)).when(d.ai_cleanup, |x| x.child(ai_prompt)).child(tog("ai", "dictation.ai_cleanup", d.ai_cleanup))).into_any_element(),
            ]))
            .child(ui::caption("Replacements"))
            .child(ui::card(reps))
            .child(div().pl(px(16.)).pt(px(6.)).text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child("Always written the way you want, e.g. “gonna” → “going to”. Names and jargon also go in Transcription → Personal Jargon."))
            .when(stenly_core::dictation::voiceink_dir().is_some(), |x| {
                let ctrl = ctrl.clone();
                let note = ctrl.read(cx).import_note.clone();
                x.child(ui::caption("VoiceInk")).child(ui::card(vec![ui::row(
                    None,
                    "Import from VoiceInk",
                    Some(note.unwrap_or_else(|| "Bring over your vocabulary, replacements, microphone order and dictation history".into()).into()),
                    ui::button("vi-import", "Import", Btn::Ghost).on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.import_voiceink(cx))),
                )
                .into_any_element()]))
            })
            .child(ui::caption("History"))
            .child(ui::card(vec![{
                let ctrl = ctrl.clone();
                let cur = d.history_limit;
                ui::row(
                    None,
                    "Keep dictations",
                    Some("Text and audio, on this Mac only".into()),
                    ui::popup("hist-limit", &[(0usize, "Don't keep"), (50, "Last 50"), (200, "Last 200"), (1000, "Last 1,000")], cur, move |v, cx| ctrl.update(cx, |c, cx| c.set("dictation.history_limit", v as i64, cx))),
                )
                .into_any_element()
            }]))
            .into_any_element()
    }

    // ------------------------------------------------------------- Microphones
    pub(crate) fn microphones_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let ctrl = self.controller().clone();
        let prio = ctrl.read(cx).config.audio.mic_priority.clone();
        let mics = self.mics.clone();
        let connected = |id: &str| mics.iter().any(|m| m.id == id || m.name == id);
        let active = prio.iter().find(|p| connected(p)).cloned();
        let name_of = |id: &str| mics.iter().find(|m| m.id == id || m.name == id).map(|m| m.name.clone()).unwrap_or_else(|| id.to_string());

        let ordered: Vec<AnyElement> = prio
            .iter()
            .enumerate()
            .map(|(i, id)| {
                let n = prio.len();
                let is_on = connected(id);
                let in_use = active.as_deref() == Some(id.as_str());
                let (c1, p1) = (ctrl.clone(), prio.clone());
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .min_h(px(48.))
                    .py(px(8.))
                    .child(div().w(px(18.)).text_size(px(theme::SUBHEAD)).text_color(theme::faint()).child(format!("{}", i + 1)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(div().text_size(px(theme::BODY)).truncate().child(name_of(id)))
                            .child(div().text_size(px(theme::SUBHEAD)).text_color(if in_use { theme::green() } else { theme::muted() }).child(if in_use {
                                "In use"
                            } else if is_on {
                                "Connected"
                            } else {
                                "Not connected"
                            })),
                    )
                    .child({
                        // Reorder/remove from one "…" menu rather than three buttons.
                        let mut entries = Vec::new();
                        let mut acts: Vec<i32> = Vec::new();
                        if i > 0 {
                            entries.push(ui::MenuEntry { symbol: Some("arrow.up"), ..ui::MenuEntry::item("Move Up") });
                            acts.push(-1);
                        }
                        if i + 1 < n {
                            entries.push(ui::MenuEntry { symbol: Some("arrow.down"), ..ui::MenuEntry::item("Move Down") });
                            acts.push(1);
                        }
                        if !entries.is_empty() {
                            entries.push(ui::MenuEntry::separator());
                            acts.push(0);
                        }
                        entries.push(ui::MenuEntry::item("Remove from List"));
                        acts.push(2);
                        let (ctrl, prio) = (c1, p1);
                        ui::more_button(("mic-more", i), entries, move |k, _, cx| {
                            let mut v = prio.clone();
                            match acts[k] {
                                -1 => v.swap(i, i - 1),
                                1 => v.swap(i, i + 1),
                                2 => {
                                    v.remove(i);
                                }
                                _ => return,
                            }
                            ctrl.update(cx, |c, cx| c.set_mic_priority(&v, cx))
                        })
                    })
                    .into_any_element()
            })
            .collect();

        let others: Vec<AnyElement> = mics
            .iter()
            .filter(|m| !prio.iter().any(|p| *p == m.id || *p == m.name))
            .enumerate()
            .map(|(i, m)| {
                let ctrl = ctrl.clone();
                let mut v = prio.clone();
                v.push(m.name.clone());
                let detail = match (m.is_default, m.is_virtual) {
                    (true, _) => "System default",
                    (_, true) => "Virtual device (no voice)",
                    _ => "Connected",
                };
                ui::row(None, m.name.clone(), Some(detail.into()), ui::button(("mic-add", i), "Add", Btn::Ghost).on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.set_mic_priority(&v, cx)))).into_any_element()
            })
            .collect();

        let summary = match &active {
            Some(a) => format!("Recording from {} (first connected in your list).", name_of(a)),
            None if prio.is_empty() => "No list yet: Stenly uses the system default microphone (or another mic when the laptop lid is closed).".to_string(),
            None => "None of your microphones are connected: using the system default.".to_string(),
        };
        div()
            .flex()
            .flex_col()
            .child(div().pt(px(12.)).text_size(px(theme::BODY)).text_color(theme::muted()).child(summary))
            .child(ui::caption("Priority"))
            .child(if ordered.is_empty() {
                ui::card(vec![div().min_h(px(48.)).flex().items_center().text_size(px(theme::BODY)).text_color(theme::muted()).child("Add microphones below, most preferred first.").into_any_element()])
            } else {
                ui::card(ordered)
            })
            .when(!others.is_empty(), |x| x.child(ui::caption("Available")).child(ui::card(others)))
            .child(div().pt(px(10.)).flex().child({
                let this = cx.entity().downgrade();
                ui::button("mic-refresh", "Refresh", Btn::Ghost).on_click(move |_, _, cx| {
                    let _ = this.update(cx, |w, cx| w.refresh_mics(cx));
                })
            }))
            .into_any_element()
    }

    // ------------------------------------------------------------- History
    pub(crate) fn history_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let q = self.history_query.read(cx).text().trim().to_lowercase();
        let items: Vec<&stenly_core::dictation::Dictation> = self.history.iter().filter(|d| q.is_empty() || d.output().to_lowercase().contains(&q) || d.app.as_deref().is_some_and(|a| a.to_lowercase().contains(&q))).take(300).collect();
        let today = Local::now().date_naive();
        let total_words: usize = self.history.iter().map(|d| d.output().split_whitespace().count()).sum();
        let total_secs: f32 = self.history.iter().map(|d| d.duration).sum();
        let n = items.len();
        let rows: Vec<AnyElement> = items
            .into_iter()
            .enumerate()
            .map(|(i, d)| {
                let when = if d.at.date_naive() == today {
                    d.at.format("%-I:%M %p").to_string()
                } else if Some(d.at.date_naive()) == today.pred_opt() {
                    format!("Yesterday {}", d.at.format("%-I:%M %p"))
                } else {
                    d.at.format("%b %-d, %-I:%M %p").to_string()
                };
                let meta = match &d.app {
                    Some(a) => format!("{when} · {a} · {:.0}s", d.duration),
                    None => format!("{when} · {:.0}s", d.duration),
                };
                let text = d.output().to_string();
                let at = d.at;
                let expanded = self.expanded_dictations.contains(&at);
                let overflowing = self.overflowing_dictations.contains(&at);
                let disclosure_id = format!("dict-disclosure-{}", at.to_rfc3339());
                let measure_this = cx.entity().downgrade();
                let this = cx.entity().downgrade();
                let last = i + 1 == n;
                div()
                    .id(("dict", i))
                    .group("dict")
                    .relative()
                    .flex()
                    .items_start()
                    .gap(px(12.))
                    .px(px(12.))
                    .py(px(10.))
                    .rounded(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(3.))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .when(!expanded, |x| x.max_h(px(DICTATION_PREVIEW_HEIGHT)).overflow_hidden())
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .text_size(px(theme::BODY))
                                            .line_height(px(19.))
                                            .child(gpui::Text::new(format!("dict-text-{}", at.to_rfc3339()).into(), text.clone().into())),
                                    )
                                    // Measure actual wrapped text, including explicit newlines.
                                    // Keep measuring while expanded and after a window resize.
                                    .on_children_prepainted(move |bounds, _, cx| {
                                        let Some(bounds) = bounds.first() else { return };
                                        let overflows = bounds.size.height > px(DICTATION_PREVIEW_HEIGHT);
                                        let Some(this) = measure_this.upgrade() else { return };
                                        if this.read(cx).overflowing_dictations.contains(&at) != overflows {
                                            cx.defer(move |cx| {
                                                this.update(cx, |w, cx| {
                                                    if overflows {
                                                        w.overflowing_dictations.insert(at);
                                                    } else {
                                                        w.overflowing_dictations.remove(&at);
                                                    }
                                                    cx.notify();
                                                });
                                            });
                                        }
                                    }),
                            )
                            .when(overflowing, |x| {
                                x.child(
                                    div()
                                        .id(SharedString::from(disclosure_id.clone()))
                                        .accessibility_id(disclosure_id)
                                        .role(gpui::Role::Button)
                                        .aria_label(if expanded { "Show less" } else { "Show more" })
                                        .aria_description(format!("Dictation from {meta}"))
                                        .aria_expanded(expanded)
                                        .focusable()
                                        .tab_stop(true)
                                        .flex()
                                        .items_center()
                                        .gap(px(4.))
                                        .h(px(24.))
                                        .px(px(4.))
                                        .rounded(px(4.))
                                        .border_1()
                                        .border_color(gpui::rgba(0x00000000))
                                        .self_start()
                                        .text_size(px(theme::SUBHEAD))
                                        .text_color(theme::accent_text())
                                        .hover(|s| s.bg(theme::hover()))
                                        .focus_visible(|s| s.bg(theme::hover()).border_color(theme::accent()))
                                        .child(icon(if expanded { icons::CHEVRON_DOWN } else { icons::CHEVRON_RIGHT }, 10., theme::accent()))
                                        .child(if expanded { "Show less" } else { "Show more" })
                                        .on_click(cx.listener(move |w, _, _, cx| w.toggle_dictation(at, cx)))
                                        .on_key_down(cx.listener(move |w, event: &gpui::KeyDownEvent, _, cx| {
                                            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                                w.toggle_dictation(at, cx);
                                                cx.stop_propagation();
                                            }
                                        })),
                                )
                            })
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .text_size(px(theme::SUBHEAD))
                                    .text_color(theme::muted())
                                    .child(meta)
                                    .when(d.enhanced.is_some(), |x| x.child(icon(icons::SPARKLES, 10., theme::muted()))),
                            ),
                    )
                    .child({
                        // One "…" per row (System Settings → Wi-Fi), not a cluster of buttons.
                        let can_redo = d.audio.as_ref().is_some_and(|a| a.exists());
                        let mut entries = vec![ui::MenuEntry { symbol: Some("doc.on.doc"), ..ui::MenuEntry::item("Copy") }];
                        if can_redo {
                            entries.push(ui::MenuEntry { symbol: Some("arrow.clockwise"), ..ui::MenuEntry::item("Transcribe Again") });
                        }
                        entries.push(ui::MenuEntry::separator());
                        entries.push(ui::MenuEntry { symbol: Some("trash"), ..ui::MenuEntry::item("Delete") });
                        let (text, entry, this) = (text.clone(), d.clone(), this.clone());
                        let picks: Vec<&'static str> = entries.iter().map(|e| match e.title.as_str() {
                            "Copy" => "copy",
                            "Transcribe Again" => "redo",
                            "Delete" => "delete",
                            _ => "",
                        }).collect();
                        ui::more_button(("dict-more", i), entries, move |i, _, cx| match picks[i] {
                            "copy" => {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
                                let _ = this.update(cx, |w, cx| w.flash("Copied", cx));
                            }
                            "redo" => {
                                let Some(dict) = crate::dictation_entity() else { return };
                                let this = this.clone();
                                dict.update(cx, |d, cx| {
                                    d.retranscribe(entry.clone(), cx, move |_, cx| {
                                        let _ = this.update(cx, |w, cx| {
                                            w.history = stenly_core::dictation::history();
                                            cx.notify();
                                        });
                                    })
                                });
                            }
                            "delete" => {
                                let _ = stenly_core::dictation::delete(at);
                                let _ = this.update(cx, |w, cx| {
                                    w.history = stenly_core::dictation::history();
                                    w.expanded_dictations.remove(&at);
                                    w.overflowing_dictations.remove(&at);
                                    cx.notify();
                                });
                            }
                            _ => {}
                        })
                        .invisible()
                        .group_hover("dict", |s| s.visible())
                    })
                    .when(!last, |x| x.child(div().absolute().bottom_0().left(px(12.)).right(px(12.)).h(px(1.)).bg(theme::divider())))
                    .into_any_element()
            })
            .collect();
        let empty = rows.is_empty();
        let shortcut = Self::shortcut_label(&self.controller().read(cx).config.dictation.shortcut);
        div()
            .flex()
            .flex_col()
            .child(div().pt(px(12.)).flex().child(ui::text_field(self.history_query.clone())))
            .when(!self.history.is_empty(), |x| {
                let dur = if total_secs < 60. { format!("{:.0} sec", total_secs) } else if total_secs < 3600. { format!("{:.0} min", total_secs / 60.) } else { format!("{:.1} h", total_secs / 3600.) };
                let n = self.history.len();
                x.child(div().pl(px(16.)).pt(px(8.)).text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child(format!("{n} dictation{} · {} words · {dur}", if n == 1 { "" } else { "s" }, total_words)))
            })
            .child(div().pt(px(12.)).child(if empty {
                ui::card(vec![
                    div()
                        .min_h(px(64.))
                        .flex()
                        .flex_col()
                        .justify_center()
                        .gap(px(2.))
                        .child(div().text_size(px(theme::BODY)).text_color(theme::muted()).child(if q.is_empty() { "No dictations yet" } else { "Nothing matches" }))
                        .when(q.is_empty(), |x| x.child(div().text_size(px(theme::SUBHEAD)).text_color(theme::faint()).child(format!("Press {shortcut} in any app and start talking."))))
                        .into_any_element(),
                ])
            } else {
                div().rounded(px(12.)).bg(theme::card()).p(px(4.)).children(rows)
            }))
            .into_any_element()
    }

    fn toggle_dictation(&mut self, at: chrono::DateTime<Local>, cx: &mut Context<Self>) {
        if !self.expanded_dictations.remove(&at) {
            self.expanded_dictations.insert(at);
        }
        cx.notify();
    }

    pub(crate) fn shortcut_label(s: &str) -> String {
        stenly_core::platform::Hotkey::parse(s).map(|h| h.label()).unwrap_or_else(|| "your shortcut".into())
    }
}
