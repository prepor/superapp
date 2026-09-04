//! The window, and the app root that hangs every app's templates on the
//! stage.
//!
//! This is one of the two files outside `shell/` that name an app, and it is
//! where the naming belongs: the crate root is the only place that knows
//! which apps exist. `AppMain::script_mod` installs that list and calls the
//! shell's block, then each app's, then its own — and its own is the window,
//! whose stage carries one named child per panel template in the build.

use makepad_widgets::*;

use crate::shell;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    startup() do #(App::script_component(vm)){
        ui: Root{
            main_window := Window{
                show_caption_bar: false
                window.title: "superapp"
                window.inner_size: vec2(1440, 900)
                pass.clear_color: #ffffffff
                body +: {
                    // Both roots fill the window; argv decides which one
                    // boots and draws (`--library`).
                    flow: Overlay
                    stage := mod.widgets.Stage{
                        // Retained content templates: named children of a
                        // custom-drawn widget are never auto-drawn — they
                        // are collected as templates and instantiated per
                        // slot, PortalList-style.
                        // Mail's nine tags. All four mailboxes draw with one
                        // widget, hung four times: a template is
                        // instantiated per slot, and the four lists are four
                        // panels.
                        mail_inbox_tpl := mod.widgets.MailMailboxPanel{}
                        mail_archive_tpl := mod.widgets.MailMailboxPanel{}
                        mail_sent_tpl := mod.widgets.MailMailboxPanel{}
                        mail_spam_tpl := mod.widgets.MailMailboxPanel{}
                        mail_message_tpl := mod.widgets.MailMessagePanel{}
                        mail_compose_tpl := mod.widgets.MailComposePanel{}
                        mail_contact_tpl := mod.widgets.MailContactPanel{}
                        mail_attachment_tpl := mod.widgets.MailAttachmentPanel{}
                        mail_settings_tpl := mod.widgets.MailSettingsPanel{}
                        mail_add_account_tpl := mod.widgets.MailAddAccountPanel{}
                        // Files' two tags: a directory is a list, a file is
                        // a card.
                        files_dir_tpl := mod.widgets.FilesDirPanel{}
                        files_card_tpl := mod.widgets.FilesCardPanel{}
                        // The agent's two tags. Bare views until the chat
                        // draws: phase 1 is the engine, and the shell
                        // refuses a registered tag with no template.
                        agent_chat_tpl := View{}
                        agent_agents_tpl := View{}
                        sys_help_tpl := mod.widgets.SysHelpPanel{}
                        sys_about_tpl := mod.widgets.SysAboutPanel{}
                        sys_effects_tpl := mod.widgets.SysEffectsPanel{}
                        sys_job_tpl := mod.widgets.SysJobPanel{}
                        sys_problems_tpl := mod.widgets.SysProblemsPanel{}
                        sys_search_tpl := mod.widgets.SysSearchPanel{}
                        sys_bucket_tpl := mod.widgets.SysBucketPanel{}
                        sys_missing_tpl := mod.widgets.SysMissingPanel{}
                        // The modal overlays are hosted the same way, keyed
                        // by a reserved slot rather than a panel.
                        rows_overlay_tpl := mod.widgets.RowsOverlay{}
                        launcher_overlay_tpl := mod.widgets.LauncherOverlay{}
                    }
                    // The panels library. Templates, never auto-drawn: a
                    // component node is instantiated from its widget's, a
                    // panel or workspace node from the stage's — exactly as
                    // panels are from theirs. Every app's fixtures are hung
                    // here, because the binary is the one place that knows
                    // which apps exist.
                    library := mod.widgets.Library{
                        link_tpl := mod.widgets.SLink{}
                        overlay_row_tpl := mod.widgets.OverlayRow{}
                        launcher_overlay_tpl := mod.widgets.LauncherOverlay{}
                        mail_row_tpl := mod.widgets.MailMailboxRow{}
                        files_row_tpl := mod.widgets.FilesDirRow{}
                        files_card_tpl := mod.widgets.CardFile{}
                        stage_tpl := mod.widgets.Stage{
                            mail_inbox_tpl := mod.widgets.MailMailboxPanel{}
                            mail_archive_tpl := mod.widgets.MailMailboxPanel{}
                            mail_sent_tpl := mod.widgets.MailMailboxPanel{}
                            mail_spam_tpl := mod.widgets.MailMailboxPanel{}
                            mail_message_tpl := mod.widgets.MailMessagePanel{}
                            mail_compose_tpl := mod.widgets.MailComposePanel{}
                            mail_contact_tpl := mod.widgets.MailContactPanel{}
                            mail_attachment_tpl := mod.widgets.MailAttachmentPanel{}
                            mail_settings_tpl := mod.widgets.MailSettingsPanel{}
                            mail_add_account_tpl := mod.widgets.MailAddAccountPanel{}
                            files_dir_tpl := mod.widgets.FilesDirPanel{}
                            files_card_tpl := mod.widgets.FilesCardPanel{}
                            agent_chat_tpl := View{}
                            agent_agents_tpl := View{}
                            sys_help_tpl := mod.widgets.SysHelpPanel{}
                            sys_about_tpl := mod.widgets.SysAboutPanel{}
                            sys_effects_tpl := mod.widgets.SysEffectsPanel{}
                            sys_job_tpl := mod.widgets.SysJobPanel{}
                            sys_problems_tpl := mod.widgets.SysProblemsPanel{}
                            sys_search_tpl := mod.widgets.SysSearchPanel{}
                            sys_bucket_tpl := mod.widgets.SysBucketPanel{}
                            sys_missing_tpl := mod.widgets.SysMissingPanel{}
                            rows_overlay_tpl := mod.widgets.RowsOverlay{}
                            launcher_overlay_tpl := mod.widgets.LauncherOverlay{}
                        }
                    }
                }
            }
        }
    }
}

/// The frame the window asks for: the display's visible frame, unless
/// `--window WxH` shrinks it for a phone-screen preview.
#[cfg(all(target_os = "macos", not(headless)))]
fn desired_frame() -> (DVec2, DVec2) {
    let (pos, size) = crate::platform::mac::visible_frame();
    match shell::boot::config().window {
        Some((w, h)) => (pos, dvec2(w.min(size.x), h.min(size.y))),
        None => (pos, size),
    }
}

/// A scripted run stays behind every normal window: it must not take the
/// screen from whoever is using the Mac — unless `--front` asks.
#[cfg(all(target_os = "macos", not(headless)))]
fn background_run() -> bool {
    shell::boot::background_run()
}

/// The makepad application root.
#[derive(Script, ScriptHook)]
pub struct App {
    #[live]
    ui: WidgetRef,
    /// The panels library is up over the workspace (⇧⌘L, or `--library`
    /// from the start).
    #[rust]
    library_shown: bool,
    /// The window wears the frame it was asked for; until it does, every
    /// frame asks again. See [`App::keep_shape`].
    #[cfg(all(target_os = "macos", not(headless)))]
    #[rust]
    shaped: bool,
    #[cfg(all(target_os = "macos", not(headless)))]
    #[rust]
    shape_tries: u32,
}

impl App {
    /// A borderless window over the display's visible frame: the menu bar
    /// and the Dock stay visible, and it is not a full-screen Space.
    #[cfg(all(target_os = "macos", not(headless)))]
    fn shape(&mut self, cx: &mut Cx) {
        let win = self.ui.window(cx, ids!(main_window));
        win.configure_macos_window(
            cx,
            MacosWindowConfig {
                chrome: MacosWindowChrome::Borderless,
                resizable: false,
                miniaturizable: false,
                ..MacosWindowConfig::default()
            },
        );
        let (pos, size) = desired_frame();
        win.configure_window(cx, size, pos, false, "superapp".to_string());
        if !background_run() {
            crate::platform::mac::activate();
        }
    }

    /// Nowhere else is there a window to shape — a headless build draws
    /// into a buffer — so `--window WxH` is the whole of it.
    #[cfg(any(not(target_os = "macos"), headless))]
    fn shape(&mut self, cx: &mut Cx) {
        if let Some((w, h)) = shell::boot::config().window {
            self.ui
                .window(cx, ids!(main_window))
                .resize(cx, dvec2(w, h));
        }
    }

    /// The window may not exist yet when [`App::shape`] asks, so the frame
    /// is asked for again every frame until it takes, or 240 go by.
    #[cfg(all(target_os = "macos", not(headless)))]
    fn keep_shape(&mut self, cx: &mut Cx, event: &Event) {
        if self.shaped || self.shape_tries >= 240 {
            return;
        }
        if let Event::NextFrame(_) | Event::Draw(_) = event {
            self.shape_tries += 1;
            let win = self.ui.window(cx, ids!(main_window));
            if win.window_id().is_none() {
                return;
            }
            let (pos, size) = desired_frame();
            let cur = win.get_inner_size(cx);
            if (cur.x - size.x).abs() > 1.0 || (cur.y - size.y).abs() > 1.0 {
                win.resize(cx, size);
                win.reposition(cx, pos);
            } else {
                self.shaped = true;
                if background_run() {
                    crate::platform::mac::configure_background_window();
                } else {
                    crate::platform::mac::activate();
                }
            }
        }
    }
}

impl App {
    /// Puts the panels library up over the workspace, or away again. The
    /// stage underneath is suspended rather than torn down — its store and
    /// its workers keep running — and comes up on first need: opened on the
    /// library, the window has no workspace until it is asked for one.
    fn show_library(&mut self, cx: &mut Cx, on: bool) {
        self.library_shown = on;
        let stage = self.ui.widget(cx, ids!(stage));
        let library = self.ui.widget(cx, ids!(library));
        if on {
            if let Some(mut st) = stage.borrow_mut::<shell::stage::Stage>() {
                st.set_suspended(cx, true);
            }
            if let Some(mut lib) = library.borrow_mut::<shell::library::Library>() {
                lib.show(cx);
            }
        } else {
            if let Some(mut lib) = library.borrow_mut::<shell::library::Library>() {
                lib.hide(cx);
            }
            let boot = stage
                .borrow::<shell::stage::Stage>()
                .is_some_and(|st| !st.booted())
                .then(shell::boot::Boot::from_argv);
            if let Some(mut st) = stage.borrow_mut::<shell::stage::Stage>() {
                st.set_suspended(cx, false);
                if let Some(boot) = boot {
                    st.boot(cx, boot);
                }
            }
        }
        cx.redraw_all();
    }
}

impl MatchEvent for App {
    fn handle_startup(&mut self, cx: &mut Cx) {
        self.shape(cx);
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        // The startup event, which every platform's entry point passes
        // through — so this is where the app list is installed, and android,
        // which has no `fn main` to install it from, gets it too.
        crate::install();
        makepad_widgets::script_mod(vm);
        shell::script_mod(vm);
        for ui in shell::uis() {
            ui.script_mod(vm);
        }
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.match_event(cx, event);
        match event {
            // Opened on the library: it is up from the first frame, and the
            // workspace stays unbooted until the toggle asks for it. The
            // menu bar gets the Dev menu now — the stage that usually builds
            // the menus has not booted, and without it the toggle back would
            // have no item to live in.
            Event::Startup if shell::boot::library_filter().is_some() => {
                shell::menu::dev_menu(cx);
                self.show_library(cx, true);
            }
            Event::Actions(actions)
                if actions
                    .iter()
                    .any(|a| a.downcast_ref::<shell::library::DevAction>().is_some()) =>
            {
                let on = !self.library_shown;
                self.show_library(cx, on);
            }
            _ => {}
        }
        self.ui.handle_event(cx, event, &mut Scope::empty());
        #[cfg(all(target_os = "macos", not(headless)))]
        self.keep_shape(cx, event);
    }
}

/// The hook the desktop entry point calls: `app_main!` generates the real
/// `fn main` just below, and the same macro generates android's
/// `activityOnCreate` symbol instead, which nothing calls `run` for.
#[cfg(not(target_os = "android"))]
pub fn run() {
    main();
}

app_main!(App);
