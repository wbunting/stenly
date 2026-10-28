//! First-run setup: welcome → permissions → calendar → models → notes → done.

use crate::controller::{Controller, KodyState};
use crate::icons::{self, icon};
use crate::main_window;
use crate::text_input::{InputEvent, TextInput};
use crate::theme;
use crate::ui::{self, Btn};
use gpui::{
    Focusable,
    AnyElement, App, Bounds, Context, Entity, FontWeight, IntoElement, ParentElement, PathPromptOptions, Render, Styled,
    Subscription, TitlebarOptions, Window, WindowBounds, WindowHandle, WindowOptions, div, prelude::*, px, size,
};
use stenly_core::platform::{Permission, PermissionState};
use stenly_core::util::collapse_tilde;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Step {
    Welcome,
    Permissions,
    Calendar,
    Models,
    Notes,
    Done,
}

const STEPS: [Step; 6] = [Step::Welcome, Step::Permissions, Step::Calendar, Step::Models, Step::Notes, Step::Done];

pub struct Onboarding {
    controller: Entity<Controller>,
    step: Step,
    kody_url: Entity<TextInput>,
    api_endpoint: Entity<TextInput>,
    api_model: Entity<TextInput>,
    api_key: Entity<TextInput>,
    _subs: Vec<Subscription>,
}

pub fn open(controller: Entity<Controller>, cx: &mut App) -> Option<WindowHandle<Onboarding>> {
    // A Dock icon while setting up: system permission prompts otherwise leave
    // an accessory app stranded behind other windows.
    controller.read(cx).shell.set_foreground_app();
    let bounds = Bounds::centered(None, size(px(720.), px(720.)), cx);
    let h = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions { title: Some("Welcome to Stenly".into()), appears_transparent: true, traffic_light_position: Some(gpui::point(px(16.), px(16.))) }),
                is_resizable: false,
                app_id: Some("stenly".into()),
                ..Default::default()
            },
            |window, cx| {
                crate::configure_native(window, controller.read(cx).shell.as_ref());
                cx.new(|cx| {
                    let sub = cx.observe(&controller, |_: &mut Onboarding, _, cx| cx.notify());
                    let appearance = cx.observe_window_appearance(window, |_: &mut Onboarding, _, cx| {
                        theme::sync(cx);
                        cx.notify();
                    });
                    let ctrl_act = controller.clone();
                    let act = cx.observe_window_activation(window, move |_: &mut Onboarding, window, cx| {
                        if window.is_window_active() {
                            ctrl_act.update(cx, |c, cx| {
                                c.refresh_permissions();
                                cx.notify();
                            });
                        }
                    });
                    let kody_url = cx.new(|cx| {
                        let mut t = TextInput::new(cx, "https://your-kody.example.com");
                        t.set_text(main_window::default_kody_url(), cx);
                        t
                    });
                    let api_cfg = controller.read(cx).config.llm.api.clone();
                    let api_endpoint = cx.new(|cx| {
                        let mut t = TextInput::new(cx, "https://api.openai.com/v1");
                        t.set_text(api_cfg.endpoint, cx);
                        t
                    });
                    let api_model = cx.new(|cx| {
                        let mut t = TextInput::new(cx, "gpt-5-mini");
                        t.set_text(api_cfg.model, cx);
                        t
                    });
                    let api_key = cx.new(|cx| TextInput::new(cx, "API key (stored in Keychain)").masked());
                    let mut subs = vec![sub, act, appearance];
                    for (input, key) in [(api_endpoint.clone(), "llm.api.endpoint"), (api_model.clone(), "llm.api.model")] {
                        let ctrl = controller.clone();
                        let save_input = input.clone();
                        let save = move |cx: &mut App| {
                            let value = save_input.read(cx).text().trim().to_string();
                            let current = {
                                let api = &ctrl.read(cx).config.llm.api;
                                if key.ends_with("endpoint") { api.endpoint.clone() } else { api.model.clone() }
                            };
                            if value != current {
                                ctrl.update(cx, |c, cx| {
                                    c.set(key, value, cx);
                                    c.api_test = crate::controller::ApiTest::Idle;
                                });
                            }
                        };
                        let submit = save.clone();
                        subs.push(cx.subscribe(&input, move |_, _, event: &InputEvent, cx| {
                            if matches!(event, InputEvent::Submit) {
                                submit(cx);
                            }
                        }));
                        let focus = input.read(cx).focus_handle(cx);
                        subs.push(cx.on_blur(&focus, window, move |_, _, cx| save(cx)));
                    }
                    {
                        let ctrl = controller.clone();
                        let save = move |input: &Entity<TextInput>, cx: &mut App| {
                            let value = input.read(cx).text().trim().to_string();
                            if !value.is_empty() {
                                ctrl.update(cx, |c, cx| c.set_api_key(&value, cx));
                                input.update(cx, |t, cx| t.set_text("", cx));
                            }
                        };
                        let submit = save.clone();
                        subs.push(cx.subscribe(&api_key, move |_, input, event: &InputEvent, cx| {
                            if matches!(event, InputEvent::Submit) {
                                submit(&input, cx);
                            }
                        }));
                        let focus = api_key.read(cx).focus_handle(cx);
                        let key = api_key.clone();
                        subs.push(cx.on_blur(&focus, window, move |_, _, cx| save(&key, cx)));
                    }
                    // Poll permissions while this window is open so "Allowed" appears promptly.
                    let ctrl = controller.clone();
                    cx.spawn(async move |this, cx| loop {
                        cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                        let alive = this.update(cx, |this, cx| {
                            if matches!(this.step, Step::Permissions | Step::Models) {
                                ctrl.update(cx, |c, _| c.refresh_permissions());
                                cx.notify();
                            }
                        });
                        if alive.is_err() {
                            break;
                        }
                    })
                    .detach();
                    // Dev: STENLY_ONBOARDING=<step index> jumps straight to a step.
                    let step = std::env::var("STENLY_ONBOARDING").ok().and_then(|v| v.parse::<usize>().ok()).and_then(|i| STEPS.get(i).copied()).unwrap_or(Step::Welcome);
                    Onboarding { controller, step, kody_url, api_endpoint, api_model, api_key, _subs: subs }
                })
            },
        )
        .ok()?;
    cx.activate(true);
    Some(h)
}

impl Onboarding {
    fn next(&mut self, cx: &mut Context<Self>) {
        let i = STEPS.iter().position(|s| *s == self.step).unwrap_or(0);
        self.step = STEPS[(i + 1).min(STEPS.len() - 1)];
        if self.step == Step::Models {
            self.controller.update(cx, |c, cx| c.refresh_ollama(cx));
        }
        cx.notify();
    }
    fn back(&mut self, cx: &mut Context<Self>) {
        let i = STEPS.iter().position(|s| *s == self.step).unwrap_or(0);
        self.step = STEPS[i.saturating_sub(1)];
        cx.notify();
    }

    fn finish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ctrl = self.controller.clone();
        ctrl.update(cx, |c, cx| c.set("app.onboarded", true, cx));
        window.remove_window();
        main_window::open(ctrl, Some(main_window::Page::Home), cx);
    }

    fn progress(&self) -> AnyElement {
        let i = STEPS.iter().position(|s| *s == self.step).unwrap_or(0);
        div()
            .flex()
            .gap(px(6.))
            .children((1..STEPS.len() - 1).map(|k| {
                div().size(px(6.)).rounded_full().bg(if k == i { theme::text() } else { theme::toggle_off() })
            }))
            .into_any_element()
    }

    fn footer(&self, primary: &'static str, primary_enabled: bool, skip: Option<&'static str>, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(ui::button("back", "Back", Btn::Ghost).on_click(cx.listener(|this, _, _, cx| this.back(cx))))
            .child(div().flex_1().flex().justify_center().child(self.progress()))
            .when_some(skip, |d, s| d.child(ui::button("skip", s, Btn::Ghost).on_click(cx.listener(|this, _, _, cx| this.next(cx)))))
            .child(
                ui::button("next", primary, if primary_enabled { Btn::Primary } else { Btn::Ghost })
                    .when(!primary_enabled, |d| d.opacity(0.5))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if primary_enabled {
                            this.next(cx)
                        }
                    })),
            )
            .into_any_element()
    }

    fn header(title: &'static str, sub: &'static str) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .pb(px(16.))
            .child(ui::page_title(title))
            .child(div().text_size(px(theme::BODY)).line_height(px(18.)).text_color(theme::muted()).child(sub))
            .into_any_element()
    }

    fn welcome(&self, cx: &mut Context<Self>) -> AnyElement {
        let point = |s: crate::icons::Sym, title: &'static str, body: &'static str| {
            div()
                .flex()
                .items_start()
                .gap(px(14.))
                .w(px(480.))
                .child(div().pt(px(2.)).child(icon(s, 22., theme::accent_text())))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .child(div().text_size(px(theme::BODY)).font_weight(FontWeight::SEMIBOLD).child(title))
                        .child(div().text_size(px(theme::BODY)).line_height(px(17.)).text_color(theme::muted()).child(body)),
                )
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .px(px(48.))
            .pt(px(56.))
            .pb(px(24.))
            .child(icons::logo(80.))
            .child(div().pt(px(14.)).child(ui::page_title("Welcome to Stenly")))
            .child(div().pt(px(6.)).text_size(px(theme::BODY)).text_color(theme::muted()).child("Meeting notes that write themselves, on your Mac."))
            .child(
                div()
                    .pt(px(30.))
                    .flex()
                    .flex_col()
                    .gap(px(18.))
                    .child(point(icons::CALENDAR, "Never miss the record button", "A minute before a call, Stenly asks whether to record. No bots join your meeting."))
                    .child(point(icons::LOCK, "Private by design", "Transcription and summaries run on this Mac. Nothing you say leaves it."))
                    .child(point(icons::FILE_TEXT, "Notes in your vault", "Each meeting becomes a Markdown note with a summary and action items.")),
            )
            .child(div().flex_1())
            .child(div().w_full().flex().justify_end().child(ui::button("start", "Continue", Btn::Primary).on_click(cx.listener(|this, _, _, cx| this.next(cx)))))
            .into_any_element()
    }

    fn permissions(&self, cx: &mut Context<Self>) -> AnyElement {
        let (req, opt) = main_window::permission_rows(&self.controller, cx);
        let c = self.controller.read(cx);
        let mic_ok = c.permission(Permission::Microphone) == PermissionState::Granted;
        div()
            .flex_1()
            .flex()
            .flex_col()
            .child(Self::header("Let Stenly Listen", "Your mic for your side of the call, system audio for everyone else. No screen recording, and nothing leaves this Mac."))
            .child(ui::card(req))
            .when(!opt.is_empty(), |d| d.child(ui::caption("Optional")).child(ui::card(opt)))
            .child(div().flex_1())
            .child(self.footer("Continue", mic_ok, Some("Later"), cx))
            .into_any_element()
    }

    fn calendar(&self, cx: &mut Context<Self>) -> AnyElement {
        let c = self.controller.read(cx);
        let eventkit = c.uses_eventkit();
        let calendar_access = c.permission(Permission::Calendars);
        let kody = c.kody.clone();
        let cals = c.calendars.clone();
        let cal_status = c.calendars_status.clone();
        let ids = c.config.calendar.calendar_ids.clone();
        let ctrl = self.controller.clone();
        let input = self.kody_url.clone();
        let connected = matches!(kody, KodyState::Connected);
        let body: AnyElement = if eventkit {
            let rows: Vec<AnyElement> = cals.iter().enumerate().map(|(i, cal)| {
                let on = ids.contains(&cal.id);
                let id = cal.id.clone();
                let ctrl = ctrl.clone();
                div().flex().items_center().gap(px(10.)).h(px(36.))
                    .child(div().size(px(10.)).rounded_full().bg(cal.color.as_deref().map(theme::hex).unwrap_or(theme::muted())))
                    .child(div().flex_1().text_size(px(theme::BODY)).child(cal.name.clone()))
                    .child(ui::toggle(("ob-eventkit-cal", i), on, move |_, _, cx| {
                        let id = id.clone();
                        ctrl.update(cx, |c, cx| c.toggle_calendar(&id, cx))
                    })).into_any_element()
            }).collect();
            match calendar_access {
                PermissionState::Granted => div().flex().flex_col().gap(px(10.))
                    .child(ui::card(vec![ui::row(Some(icons::CHECK_CIRCLE), "Using the calendars on this Mac", Some("Google, iCloud, Exchange and other accounts already in Calendar.app.".into()), div()).into_any_element()]))
                    .child(ui::caption("Which calendars should Stenly watch?"))
                    .child(div().id("eventkit-cals").max_h(px(240.)).overflow_y_scroll().child(ui::card(if rows.is_empty() {
                        match cal_status {
                            Some(Err(e)) => vec![ui::row(None, "Couldn't load your calendars", Some(friendly_err(&e).into()), ui::button("ob-eventkit-retry", "Try Again", Btn::Ghost).on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.load_calendars(cx)))).into_any_element()],
                            _ => vec![div().py(px(12.)).text_size(px(13.)).text_color(theme::muted()).child("Loading calendars…").into_any_element()],
                        }
                    } else { rows }))).into_any_element(),
                PermissionState::Denied => ui::card(vec![ui::row(Some(icons::CALENDAR), "Calendar access is off", Some("Allow Stenly in System Settings → Privacy & Security → Calendars, then return here.".into()), ui::button("ob-calendar-settings", "Open Settings", Btn::Accent).on_click(move |_, _, cx| ctrl.update(cx, |c, _| c.shell.open_permission_settings(Permission::Calendars)))).into_any_element()]).into_any_element(),
                _ => ui::card(vec![ui::row(Some(icons::CALENDAR), "Use the calendars on this Mac", Some("One permission lets Stenly see meetings in Google, iCloud and Exchange accounts already in Calendar.app. Nothing leaves your Mac.".into()), ui::button("ob-calendar-allow", "Allow Calendar Access", Btn::Primary).on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.request_calendar_access(cx)))).into_any_element()]).into_any_element(),
            }
        } else if connected {
            let rows: Vec<AnyElement> = cals
                .iter()
                .enumerate()
                .map(|(i, cal)| {
                    let on = ids.contains(&cal.id) || (ids.is_empty() && cal.primary);
                    let id = cal.id.clone();
                    let ctrl = ctrl.clone();
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .h(px(36.))
                        .child(div().size(px(10.)).rounded_full().bg(cal.color.as_deref().map(theme::hex).unwrap_or(theme::muted())))
                        .child(div().flex_1().text_size(px(theme::BODY)).child(cal.name.clone()))
                        .child(ui::toggle(("ob-cal", i), on, move |_, _, cx| {
                            let id = id.clone();
                            ctrl.update(cx, |c, cx| c.toggle_calendar(&id, cx))
                        }))
                        .into_any_element()
                })
                .collect();
            div()
                .flex()
                .flex_col()
                .child(ui::card(vec![ui::row(Some(icons::CHECK_CIRCLE), "Google Calendar connected", Some(format!("via {}", c.config.calendar.kody_url.trim_start_matches("https://")).into()), div()).into_any_element()]))
                .child(ui::caption("Which calendars should Stenly watch?"))
                .child(div().id("cals").max_h(px(240.)).overflow_y_scroll().child(ui::card(if !rows.is_empty() {
                    rows
                } else {
                    match cal_status {
                        Some(Err(e)) => {
                            let ctrl = ctrl.clone();
                            vec![ui::row(None, "Couldn't load your calendars", Some(friendly_err(&e).into()), ui::button("ob-cal-retry", "Try Again", Btn::Ghost).on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.load_calendars(cx)))).into_any_element()]
                        }
                        Some(Ok(())) => vec![div().py(px(12.)).text_size(px(13.)).text_color(theme::muted()).child("No calendars found on this account.").into_any_element()],
                        None => vec![div().flex().items_center().gap(px(10.)).py(px(12.)).text_size(px(13.)).text_color(theme::muted()).child(ui::spinner("ob-cal-spin", 14., theme::muted())).child("Loading calendars from Kody…").into_any_element()],
                    }
                })))
                .into_any_element()
        } else {
            let busy = matches!(kody, KodyState::Connecting);
            let err = if let KodyState::Failed(e) = &kody { Some(e.clone()) } else { None };
            div()
                .flex()
                .flex_col()
                .gap(px(12.))
                .child(ui::card(vec![
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(12.))
                        .py(px(16.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(14.))
                                .child(ui::icon_tile(icons::CALENDAR))
                                .child(div().flex_1().flex().flex_col().gap(px(2.)).child(div().text_size(px(14.)).child("Google Calendar via Kody")).child(div().text_size(px(12.5)).text_color(theme::muted()).child("Sign in to Kody in your browser. Kody already holds your Google connection.")))
                                .child(if busy {
                                    ui::pending("ob-kody-wait", "Waiting for browser…").into_any_element()
                                } else {
                                    ui::button("ob-connect", "Connect", Btn::Primary)
                                        .on_click(move |_, _, cx| {
                                            let url = input.read(cx).text().to_string();
                                            ctrl.update(cx, |c, cx| c.connect_kody(url, cx))
                                        })
                                        .into_any_element()
                                }),
                        )
                        .child(
                            div()
                                .ml(px(32.))
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(div().text_size(px(theme::BODY)).text_color(theme::muted()).child("Kody URL:"))
                                .child(ui::text_field(self.kody_url.clone())),
                        )
                        .into_any_element(),
                ]))
                .when_some(err, |d, e| d.child(div().px(px(2.)).text_size(px(theme::SUBHEAD)).text_color(theme::red()).child(e)))
                .child(div().px(px(2.)).text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child("You can also add a private ICS link later in Calendar settings."))
                .into_any_element()
        };
        div()
            .flex_1()
            .flex()
            .flex_col()
            .child(Self::header("Connect Your Calendar", if eventkit { "Stenly reads the calendars already on this Mac and pops up a minute before a meeting with a Zoom, Meet or Teams link." } else { "Add a private or shared ICS calendar link. Stenly will pop up a minute before meetings with a Zoom, Meet or Teams link." }))
            .child(body)
            .child(div().flex_1())
            .child(self.footer("Continue", true, if eventkit || connected { None } else { Some("Skip") }, cx))
            .into_any_element()
    }

    fn models(&self, cx: &mut Context<Self>) -> AnyElement {
        let (cfg, asr_ok, asr_dl, ollama, api_key_saved, api_test, llm_ok, llm_dl) = {
            let c = self.controller.read(cx);
            let cfg = c.config.clone();
            let asr = stenly_asr::models::find(&cfg.asr.model).unwrap_or(&stenly_asr::models::MODELS[0]);
            let llm = stenly_core::ollama::find(&cfg.llm.model).copied().unwrap_or(stenly_core::ollama::CATALOG[0]);
            (cfg, c.asr_installed(asr.id), c.download(asr.id).cloned(), c.ollama.clone(), c.api_key_saved, c.api_test.clone(), c.llm_installed(llm.id), c.download(llm.id).cloned())
        };
        let asr = stenly_asr::models::find(&cfg.asr.model).unwrap_or(&stenly_asr::models::MODELS[0]);
        let uses_api = cfg.llm.uses_api();
        let uses_cli = cfg.llm.uses_cli();
        let uses_chatgpt = cfg.llm.uses_chatgpt();
        let chatgpt = self.controller.read(cx).chatgpt.clone();
        let ctrl = self.controller.clone();
        let cli_tools = self.controller.read(cx).cli_tools.clone();

        let asr_status: AnyElement = if let Some(d) = asr_dl {
            div().flex().flex_col().items_end().gap(px(6.)).child(div().text_size(px(12.)).text_color(theme::muted()).child(format!("{:.0}%", d.fraction.max(0.0) * 100.0))).child(ui::progress_bar(d.fraction, 120.)).into_any_element()
        } else if asr_ok {
            ui::chip("Ready", Some(icons::CHECK), theme::green()).into_any_element()
        } else {
            let id = asr.id.to_string();
            let ctrl = ctrl.clone();
            ui::button_icon("ob-asr", "Download", Btn::Primary, icons::DOWNLOAD).on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.download_asr(id.clone(), cx))).into_any_element()
        };
        let llm = stenly_core::ollama::find(&cfg.llm.model).copied().unwrap_or(stenly_core::ollama::CATALOG[0]);
        let local_detail = if ollama.running && llm_ok {
            div().text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child(format!("Ollama and {} are ready · summaries stay on this Mac.", llm.name)).into_any_element()
        } else if ollama.running {
            div().text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child(format!("Ollama is ready. Download {} ({:.1} GB) to write local summaries.", llm.name, llm.size_gb)).into_any_element()
        } else {
            div().text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child("No setup needed to record: Stenly will save a transcript note. Add Ollama later for local summaries.").into_any_element()
        };
        let local_action: AnyElement = if let Some(d) = llm_dl {
            div().flex().flex_col().items_end().gap(px(6.)).child(div().text_size(px(12.)).text_color(theme::muted()).child(format!("{:.0}%", d.fraction.max(0.0) * 100.0))).child(ui::progress_bar(d.fraction, 120.)).into_any_element()
        } else if ollama.running && llm_ok {
            ui::chip("Ready", Some(icons::CHECK), theme::green()).into_any_element()
        } else if ollama.running {
            let ctrl = ctrl.clone();
            let id = llm.id.to_string();
            ui::button_icon("ob-llm", "Download", Btn::Primary, icons::DOWNLOAD).on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.pull_llm(id.clone(), cx))).into_any_element()
        } else {
            let ctrl = ctrl.clone();
            div().flex().gap(px(8.)).child(ui::button("ob-get-ollama", "Get Ollama", Btn::Ghost).on_click(|_, _, cx| cx.open_url("https://ollama.com/download"))).child(ui::button("ob-recheck", "Check", Btn::Ghost).on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.refresh_ollama(cx)))).into_any_element()
        };
        let select_local = {
            let ctrl = ctrl.clone();
            let other = uses_api || uses_cli || uses_chatgpt;
            ui::button("ob-use-local", if other { "Use Local" } else { "Selected" }, if other { Btn::Outline } else { Btn::Ghost })
                .when(other, |b| b.on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.set("llm.provider", "ollama", cx))))
        };
        // Sign in with ChatGPT: signing in also selects it
        let select_chatgpt: AnyElement = {
            use crate::controller::ChatGptState;
            let ctrl = ctrl.clone();
            match &chatgpt {
                ChatGptState::SigningIn => ui::pending("ob-chatgpt-wait", "Waiting for browser…").into_any_element(),
                ChatGptState::SignedIn(_) if uses_chatgpt => ui::button("ob-use-chatgpt", "Selected", Btn::Ghost).into_any_element(),
                ChatGptState::SignedIn(_) => ui::button("ob-use-chatgpt", "Use ChatGPT", Btn::Outline).on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.set("llm.provider", "chatgpt", cx))).into_any_element(),
                _ => ui::button("ob-use-chatgpt", "Sign In", Btn::Outline).on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.sign_in_chatgpt(cx))).into_any_element(),
            }
        };
        let chatgpt_desc: gpui::SharedString = match &chatgpt {
            crate::controller::ChatGptState::SignedIn(who) => format!("Signed in as {who}. Your plan pays; no API key.").into(),
            crate::controller::ChatGptState::Failed(e) => e.clone().into(),
            _ => "Your Plus or Pro plan pays; no API key.".into(),
        };
        let select_api = {
            let ctrl = ctrl.clone();
            ui::button("ob-use-api", if uses_api { "Selected" } else { "Use Cloud" }, if uses_api { Btn::Ghost } else { Btn::Outline })
                .when(!uses_api, |b| b.on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.set("llm.provider", "api", cx))))
        };
        // Codex / Claude Code, when installed: their own sign-in pays, Stenly holds no token.
        let cli_rows: Vec<AnyElement> = cli_tools
            .iter()
            .map(|d| {
                let tool = d.tool;
                let selected = uses_cli && stenly_core::cli_llm::Tool::parse(&cfg.llm.cli.tool).is_none_or(|t| t == tool);
                let ctrl = ctrl.clone();
                let id: &'static str = match tool {
                    stenly_core::cli_llm::Tool::Codex => "ob-use-codex",
                    stenly_core::cli_llm::Tool::Claude => "ob-use-claude",
                };
                let label = if selected { "Selected".to_string() } else { format!("Use {}", tool.label()) };
                ui::row(
                    Some(icons::CODE),
                    format!("{} · {}", tool.label(), tool.vendor()),
                    Some("Already installed here. Uses the account it is signed in to.".into()),
                    ui::button(id, label, if selected { Btn::Ghost } else { Btn::Outline })
                        .when(!selected, |b| b.on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.use_cli_tool(tool, cx)))),
                )
                .into_any_element()
            })
            .collect();
        let chosen_cli = stenly_core::cli_llm::Tool::parse(&cfg.llm.cli.tool).or_else(|| cli_tools.first().map(|d| d.tool));
        const PRESETS: &[(&str, &str, &str)] = &[
            ("OpenAI", "https://api.openai.com/v1", "gpt-5-mini"),
            ("Anthropic", "https://api.anthropic.com/v1", "claude-sonnet-4-6"),
            ("OpenRouter", "https://openrouter.ai/api/v1", "anthropic/claude-sonnet-4.6"),
            ("Groq", "https://api.groq.com/openai/v1", "openai/gpt-oss-120b"),
            ("Google Gemini", "https://generativelanguage.googleapis.com/v1beta/openai", "gemini-2.5-flash"),
            ("Custom", "", ""),
        ];
        let api = cfg.llm.api.clone();
        let preset = PRESETS.iter().position(|(_, endpoint, _)| !endpoint.is_empty() && api.endpoint.trim_end_matches('/') == *endpoint).unwrap_or(PRESETS.len() - 1);
        let endpoint = self.api_endpoint.clone();
        let model = self.api_model.clone();
        let provider_picker = {
            let ctrl = ctrl.clone();
            ui::popup("ob-api-provider", &PRESETS.iter().enumerate().map(|(i, p)| (i, p.0)).collect::<Vec<_>>(), preset, move |i, cx| {
                let (_, url, name) = PRESETS[i];
                if url.is_empty() { return; }
                endpoint.update(cx, |t, cx| t.set_text(url, cx));
                model.update(cx, |t, cx| t.set_text(name, cx));
                ctrl.update(cx, |c, cx| {
                    c.set("llm.api.endpoint", url, cx);
                    c.set("llm.api.model", name, cx);
                    c.api_test = crate::controller::ApiTest::Idle;
                });
            })
        };
        let field = |label: &'static str, input: Entity<TextInput>| div().flex().items_center().gap(px(12.)).min_h(px(40.)).child(div().w(px(78.)).flex_shrink_0().text_size(px(theme::BODY)).child(label)).child(ui::text_field(input));
        let api_status: AnyElement = match api_test {
            crate::controller::ApiTest::Idle => div().text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child(if api_key_saved { "Key is saved in your Keychain." } else { "Your key is stored in the Keychain, never in config.toml." }).into_any_element(),
            crate::controller::ApiTest::Running => ui::pending("ob-api-test", "Testing…").into_any_element(),
            crate::controller::ApiTest::Ok(msg) => div().text_size(px(theme::SUBHEAD)).text_color(theme::green()).child(msg).into_any_element(),
            crate::controller::ApiTest::Failed(msg) => div().text_size(px(theme::SUBHEAD)).text_color(theme::amber()).child(msg).into_any_element(),
        };
        let api_form = div().pt(px(12.)).child(ui::card(vec![
            ui::row(None, "Provider", None, provider_picker).into_any_element(),
            field("Base URL", self.api_endpoint.clone()).into_any_element(),
            field("Model", self.api_model.clone()).into_any_element(),
            field("API key", self.api_key.clone()).into_any_element(),
            div().flex().items_center().gap(px(12.)).min_h(px(42.)).child(div().flex_1().min_w_0().child(api_status)).child({ let ctrl = ctrl.clone(); ui::button("ob-api-test", "Test", Btn::Outline).on_click(move |_, window, cx| { window.blur(cx); let ctrl = ctrl.clone(); cx.defer(move |cx| ctrl.update(cx, |c, cx| c.test_api(cx))); }) }).into_any_element(),
        ]));

        div().flex_1().flex().flex_col()
            .child(Self::header("Notes, with or without AI", "Transcription runs on this Mac. Your first meeting always becomes a Markdown note with attendees and a transcript."))
            .child(ui::card(vec![ui::row(Some(icons::WAVE), format!("{} · transcription", asr.name), Some(format!("NVIDIA · {} · {} MB", asr.language, asr.approx_mb).into()), asr_status).into_any_element()]))
            .child(ui::caption("Summaries (optional)"))
            .child(ui::card(vec![
                ui::row(Some(icons::SPARKLES), "Local · Ollama", Some("Summaries stay on this Mac.".into()), select_local).into_any_element(),
                div().px(px(16.)).pb(px(12.)).child(local_detail).child(div().pt(px(8.)).child(local_action)).into_any_element(),
                ui::row(Some(icons::LOCK), "Cloud · Sign in with ChatGPT", Some(chatgpt_desc), select_chatgpt).into_any_element(),
                ui::row(Some(icons::LOCK), "Cloud · your API key", Some("Use an OpenAI-compatible provider.".into()), select_api).into_any_element(),
            ].into_iter().chain(cli_rows).collect()))
            .when_some(chosen_cli.filter(|_| uses_cli), |d, tool| d.child(div().px(px(2.)).pt(px(8.)).text_size(px(theme::SUBHEAD)).line_height(px(17.)).text_color(theme::muted()).child(tool.disclosure())))
            .when(uses_chatgpt, |d| d.child(div().px(px(2.)).pt(px(8.)).text_size(px(theme::SUBHEAD)).line_height(px(17.)).text_color(theme::muted()).child("Your transcript is sent to OpenAI under your ChatGPT account to write the summary.")))
            .when(uses_api, |d| d.child(div().px(px(2.)).pt(px(8.)).text_size(px(theme::SUBHEAD)).line_height(px(17.)).text_color(theme::muted()).child("Your transcript is sent to this provider to write the summary.")).child(api_form))
            .child(div().px(px(2.)).pt(px(8.)).text_size(px(theme::SUBHEAD)).text_color(theme::muted()).child("You can turn on summaries later in Settings → Summaries."))
            .child(div().flex_1())
            .child(self.footer("Continue", true, None, cx))
            .into_any_element()
    }

    fn notes(&self, cx: &mut Context<Self>) -> AnyElement {
        let c = self.controller.read(cx);
        let notes_dir = c.config.paths().notes_dir;
        let vaults = stenly_core::obsidian::vaults();
        let mut rows: Vec<AnyElement> = vaults
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let target = v.path.join("Meetings");
                let active = notes_dir.starts_with(&v.path);
                let ctrl = self.controller.clone();
                ui::row(
                    Some(icons::ARCHIVE),
                    if v.name.eq_ignore_ascii_case("obsidian") { "Obsidian vault".to_string() } else { format!("Obsidian · {}", v.name) },
                    Some(format!("{}/Meetings", collapse_tilde(&v.path)).into()),
                    if active {
                        ui::chip("Selected", Some(icons::CHECK), theme::green()).into_any_element()
                    } else {
                        ui::button(("ob-vault", i), "Use", Btn::Ghost).on_click(move |_, _, cx| ctrl.update(cx, |c, cx| c.set_path("output.notes_dir", &target, cx))).into_any_element()
                    },
                )
                .into_any_element()
            })
            .collect();
        let ctrl = self.controller.clone();
        rows.push(
            ui::row(
                Some(icons::FOLDER),
                "Notes folder",
                Some(collapse_tilde(&notes_dir).into()),
                ui::button("ob-pick", "Choose…", Btn::Outline).on_click(cx.listener(move |_, _, _, cx| {
                    let rx = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Choose".into()) });
                    let ctrl = ctrl.clone();
                    cx.spawn(async move |_, cx| {
                        if let Ok(Ok(Some(p))) = rx.await {
                            if let Some(p) = p.into_iter().next() {
                                let _ = ctrl.update(cx, |c, cx| c.set_path("output.notes_dir", &p, cx));
                            }
                        }
                    })
                    .detach();
                })),
            )
            .into_any_element(),
        );
        div()
            .flex_1()
            .flex()
            .flex_col()
            .child(Self::header("Choose Where Notes Go", "Each meeting becomes a Markdown file with a summary, action items and a transcript, linked from your daily note if you like."))
            .child(ui::card(rows))
            .child(div().flex_1())
            .child(self.footer("Continue", true, None, cx))
            .into_any_element()
    }

    fn done(&self, cx: &mut Context<Self>) -> AnyElement {
        let c = self.controller.read(cx);
        let next = c.next_event().map(|e| format!("Next up: {} at {}", e.title, e.start.with_timezone(&chrono::Local).format("%-I:%M %p")));
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(10.))
            .child(icons::logo(72.))
            .child(div().pt(px(6.)).child(ui::page_title("You're All Set")))
            .child(
                div()
                    .w(px(420.))
                    .text_center()
                    .text_size(px(theme::BODY))
                    .line_height(px(18.))
                    .text_color(theme::muted())
                    .child("Stenly lives in your menu bar. Before a meeting starts it'll pop up in the corner: click Record and forget about it. Notes appear when the call ends."),
            )
            .child({
                let key = stenly_core::platform::Hotkey::parse(&c.config.dictation.shortcut).map(|h| h.label());
                div()
                    .w(px(420.))
                    .pt(px(4.))
                    .text_center()
                    .text_size(px(theme::BODY))
                    .line_height(px(18.))
                    .text_color(theme::muted())
                    .when_some(key.filter(|_| c.config.dictation.enabled), |d, k| d.child(format!("To dictate anywhere, tap {k}, talk, and tap it again. Or hold it while you speak.")))
            })
            .when_some(next, |d, n| d.child(div().pt(px(4.)).text_size(px(theme::BODY)).font_weight(FontWeight::MEDIUM).text_color(theme::text()).child(n)))
            .child(div().pt(px(14.)).child(ui::button("finish", "Open Stenly", Btn::Primary).on_click(cx.listener(|this, _, window, cx| this.finish(window, cx)))))
            .into_any_element()
    }
}

impl Render for Onboarding {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.step {
            Step::Welcome => return div().size_full().bg(theme::bg()).text_color(theme::text()).font_family(theme::SANS).child(self.welcome(cx)),
            Step::Permissions => self.permissions(cx),
            Step::Calendar => self.calendar(cx),
            Step::Models => self.models(cx),
            Step::Notes => self.notes(cx),
            Step::Done => self.done(cx),
        };
        div()
            .size_full()
            .bg(theme::bg())
            .text_color(theme::text())
            .font_family(theme::SANS)
            .text_size(px(theme::BODY))
            .child(div().size_full().flex().flex_col().px(px(40.)).pt(px(48.)).pb(px(24.)).child(div().flex_1().flex().flex_col().child(content)))
    }
}

/// Turn a raw error chain into one sentence a person can act on.
fn friendly_err(e: &str) -> String {
    if e.contains("couldn't reach Kody") || e.contains("can't reach Kody") || e.contains("timeout") {
        "Kody isn't responding. Check that it's running and you're on the tailnet.".into()
    } else if e.contains("refreshing Kody session") || e.contains("unauthorized") {
        "Your Kody sign-in has expired. Disconnect and connect again.".into()
    } else if e.contains("having trouble") {
        "Kody is having trouble right now. Try again in a minute.".into()
    } else {
        e.split(": ").next().unwrap_or(e).to_string()
    }
}
