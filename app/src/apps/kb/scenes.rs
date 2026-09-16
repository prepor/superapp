//! The KB's entries for the panels library, over the seeded wiki.
//!
//! It leads with the agent, because that is the door: three chats with
//! their turns and calls seeded as rows — presentation fixtures, no tool
//! run, since the tools do not exist yet — then the catalogue, a page in
//! four states, the history and a revision, the editor, the file card in
//! its four states, and the import form in its three.

use kernel::app::Mode;
use kernel::layout::Grid;
use kernel::panel::PanelId;
use kernel::scene::Scene;
use kernel::session::Session;
use kernel::store::Store;
use kernel::time::ts;
use serde_json::{json, Value};

use crate::shell::app_ui::Setup;
use crate::shell::catalog::{panel, panel_fake, steps, workspace_on};

use super::model::{self, Where};
use super::panels::{Catalogue, Edit, File, History, Import, Page, Revision};
use super::seed::{self, hash_of};

/// The KB's scenes, in canvas order.
#[must_use]
pub fn scenes() -> Vec<Scene<Setup>> {
    vec![chat(), catalogue(), page(), history(), edit(), file(), import()]
}

/// A panel alone on the phone's grid — four by three, the cover display.
fn phone_panel(open: impl Fn(&Store) -> PanelId + 'static, script: &str) -> Setup {
    Setup::Stage {
        open: Some(std::rc::Rc::new(move |s: &Session| open(s.store()))),
        solo: true,
        steps: steps(script),
        grid: Some(Grid { w: 4, h: 3 }),
        mode: Mode::Deny,
    }
}

// -- the chats ---------------------------------------------------------------------

/// One step of a seeded conversation.
enum Turn {
    Said(&'static str),
    Call { tool: &'static str, args: Value, output: Value, label: Option<&'static str> },
    Answered(&'static str),
}

/// A conversation written straight into the agent's rows: the person's
/// turn with its chips, each call as the assistant's turn, the call's row
/// and the tool's answer, and the assistant's text at the end. The round
/// is over before the panel opens, so nothing races a worker.
fn transcript(store: &Store, title: &str, chips: Vec<Value>, turns: Vec<Turn>) -> i64 {
    use crate::apps::agent::model as agent;
    use crate::apps::agent::wire::{FunctionCall, Message, ToolCall};
    use crate::apps::agent::MODEL;
    let title = title.to_string();
    store
        .write(move |c| {
            let mut now = ts(2026, 8, 30, 17, 48);
            let chat = agent::new_chat_tx(c, &title, MODEL, now)?;
            let run = agent::new_run_tx(c, chat, now)?;
            let mut n = 0;
            for t in turns {
                now += 4.0;
                match t {
                    Turn::Said(text) => {
                        let turn = agent::Turn::new(Message::user(text))
                            .carrying(&agent::Carried { chips: chips.clone(), context: None })
                            .by(run);
                        agent::add_turn_tx(c, chat, &turn, now)?;
                    }
                    Turn::Call { tool, args, output, label } => {
                        n += 1;
                        let call = ToolCall {
                            id: format!("call_{n}"),
                            r#type: "function".into(),
                            function: FunctionCall { name: tool.into(), arguments: args.to_string() },
                        };
                        let mut message = Message::of(crate::apps::agent::wire::Role::Assistant);
                        message.tool_calls = vec![call.clone()];
                        let (turn_id, _) = agent::add_turn_tx(c, chat, &agent::Turn::new(message).by(run), now)?;
                        let id = agent::add_call_tx(c, run, turn_id, &call, now)?;
                        let out = output.to_string();
                        agent::set_call_tx(c, id, agent::CALL_DONE, &out, label, now + 1.0)?;
                        agent::add_turn_tx(c, chat, &agent::Turn::new(Message::tool(call.id.clone(), out)).by(run), now + 1.0)?;
                    }
                    Turn::Answered(text) => {
                        let turn = agent::Turn::new(Message::assistant(text)).finishing("stop").by(run);
                        agent::add_turn_tx(c, chat, &turn, now)?;
                    }
                }
            }
            agent::set_run_status_tx(c, run, agent::DONE, None, now + 2.0)?;
            agent::set_run_usage_tx(c, run, &agent::Cost { input: 4_120, output: 212, cached: 3_900 })?;
            Ok(chat)
        })
        .unwrap_or(0)
}

/// The chip a chat carries: the catalogue, or a letter's attachment.
fn chip(tag: &str, args: &[&str], title: &str, about: &str) -> Value {
    json!({ "kind": "panel", "tag": tag, "args": args, "title": title, "workspace": 1, "about": about })
}

fn search_chat(store: &Store) -> i64 {
    transcript(
        store,
        "what does my kb say about porto lume",
        vec![chip("kb", &[], "kb", "The knowledge base's catalogue: every page with its kind, title and summary.")],
        vec![
            Turn::Said("what does my kb say about porto lume"),
            Turn::Call {
                tool: "kb.search",
                args: json!({ "query": "porto lume" }),
                output: json!({ "hits": [
                    { "slug": "porto-lume", "kind": "entity", "title": "Porto Lume", "summary": "The harbour town the sailing trip starts from: the marina, the office, the chandlery." },
                    { "slug": "mooring-fees-2026", "kind": "source", "title": "Porto Lume marina — mooring fees 2026", "summary": "The marina's 2026 price list: per night by length and month, tourist tax on top." },
                    { "slug": "sailing-trip-2026", "kind": "project", "title": "Sailing the gulf, September 2026", "summary": "Ten days on a chartered 34-footer out of Porto Lume." }
                ] }),
                label: None,
            },
            Turn::Answered(
                "Three pages. **porto-lume** is the harbour town the trip starts from: a marina of two \
                 hundred berths, the office on channel 17 from 07:00, the chandlery behind the fish market. \
                 **mooring-fees-2026** has the marina's price list — a 34-footer is 62 € a night in September, \
                 tourist tax on top. **sailing-trip-2026** is the plan itself: ten days from the second week \
                 of September, the islands to the south and back by the shore.\n\nOne thing is still open there: \
                 the tide tables for the strait are linked but not filed.",
            ),
        ],
    )
}

fn file_chat(store: &Store) -> i64 {
    transcript(
        store,
        "file the tax letter",
        vec![chip(
            "attachment",
            &["12", "1"],
            "Steuerbescheid 2025.pdf",
            "One attachment of a letter: a PDF from the Finanzamt, 2 pages.",
        )],
        vec![
            Turn::Said("file this letter in the kb"),
            Turn::Call {
                tool: "kb.attach",
                args: json!({ "from": "mail:12/1", "path": "sources/steuerbescheid-2025.pdf" }),
                output: json!({ "path": "sources/steuerbescheid-2025.pdf", "hash": hash_of("sources/steuerbescheid-2025.pdf"), "size": 184_320, "queued": true }),
                label: Some("attach sources/steuerbescheid-2025.pdf"),
            },
            Turn::Call {
                tool: "kb.write",
                args: json!({
                    "slug": "steuerbescheid-2025",
                    "message": "filed the 2025 tax assessment",
                    "document": "---\ntype: source\ntitle: Steuerbescheid 2025\nsummary: The 2025 income tax assessment: 412 € to pay by 14 October 2026, no objection filed.\ntags: [taxes]\n---\n\nThe Finanzamt's assessment for 2025, dated 28 August 2026, in [the letter](sources/steuerbescheid-2025.pdf).\n\n- to pay: 412 €, by 14 October 2026\n- objection window: one month from receipt\n- see [[taxes-2025]] for the return it answers\n"
                }),
                output: json!({ "slug": "steuerbescheid-2025", "revision": hash_of("rev:steuerbescheid-2025:1"), "links": 2 }),
                label: Some("write steuerbescheid-2025"),
            },
            Turn::Answered(
                "Filed. The letter is at `sources/steuerbescheid-2025.pdf` and the page **steuerbescheid-2025** \
                 says what matters: 412 € to pay by 14 October 2026, the objection window a month from receipt. \
                 It links the return under **taxes-2025**.",
            ),
        ],
    )
}

fn remember_chat(store: &Store) -> i64 {
    transcript(
        store,
        "remember that the marina office closes at 16:00 on Sundays",
        Vec::new(),
        vec![
            Turn::Said("remember that the marina office at Porto Lume closes at 16:00 on Sundays"),
            Turn::Call {
                tool: "kb.remember",
                args: json!({ "line": "the marina office at Porto Lume closes at 16:00 on Sundays" }),
                output: json!({ "slug": "remembered", "line": "- 2026-08-30 — the marina office at Porto Lume closes at 16:00 on Sundays" }),
                label: Some("remember one line"),
            },
            Turn::Answered("Kept, dated today, on the memory page. Every chat reads it from now on."),
        ],
    )
}

fn chat() -> Scene<Setup> {
    use crate::apps::agent::Chat;
    Scene::new("kb chat", (560.0, 640.0))
        .note("The chat is the KB's front door: a question runs kb.search and answers with pages, *file this* over a letter's attachment runs kb.attach and kb.write, *remember that* runs kb.remember.")
        .note("Presentation fixtures: the turns and calls are seeded as the agent's own rows, and no tool ran — the tools are phase 2's. The cards say what they will say then.")
        .node("search", panel_fake(|s| Chat::id(search_chat(s)), ""))
        .about("what does my kb say about porto lume: a kb.search card, and an answer that names the pages")
        .node("file a letter", panel_fake(|s| Chat::id(file_chat(s)), ""))
        .about("a letter's attachment as the chip; kb.attach puts the bytes under a path, kb.write makes the page")
        .node(
            "the page beside",
            workspace_on(
                |s| Chat::id(file_chat(s)),
                "key cmd 2\nwait 400\ntype \"kb\"\nwait 400\nkey enter\nwait 700\nkey /\nwait 300\ntype \"@kind:source\"\nwait 500\nkey down\nwait 900",
            ),
        )
        .sized((1600.0, 700.0))
        .about("the same chat with the catalogue opened from the launcher and a source page previewed beside it")
        .node("remember", panel_fake(|s| Chat::id(remember_chat(s)), ""))
        .about("remember that …: one dated line appended to the memory page, no form")
        .edge("file a letter", "the page beside", "double-cmd, kb")
}

// -- the catalogue ----------------------------------------------------------------

fn catalogue() -> Scene<Setup> {
    Scene::new("kb catalogue", (560.0, 640.0))
        .note("Every page, grouped by kind under the index's captions: the title over its summary, the slug and the date muted at the right. The cursor previews a page beside the list.")
        .note("ask is first on the bar, before new page; lint opens a chat that runs kb.lint; import is a link to the form.")
        .node("default", panel(|_| Catalogue::id(), ""))
        .about("eight pages across the seven kinds")
        .node("skills", panel(|_| Catalogue::filtered("@kind:skill"), ""))
        .about("@kind:skill — the pages whose name and summary are in every brief")
        .node("orphan", panel(|_| Catalogue::filtered("@orphan"), ""))
        .about("@orphan — the one page nothing links to, an inbox note")
        .node("dangling", panel(|_| Catalogue::filtered("@dangling"), ""))
        .about("@dangling — the page whose wikilink names no page")
        .node("phone", phone_panel(|_| Catalogue::id(), ""))
        .sized((380.0, 760.0))
        .about("the same list on the cover display: the bar wraps under a thumb")
        .edge("default", "skills", "@kind:skill")
        .edge("default", "orphan", "@orphan")
        .edge("default", "dangling", "@dangling")
}

// -- a page ------------------------------------------------------------------------

fn page() -> Scene<Setup> {
    Scene::new("kb page", (560.0, 760.0))
        .note("One page as a reading: the title, a muted line — kind · tags · written · revisions — the body through the shared Html widget, and under rules what it links to, what links to it, and the files it names.")
        .note("A wikilink is a solid link that opens the page joined; a link to a file opens its card; a link to a page that is not there draws in the muted grey.")
        .node("entity", panel(|_| Page::id("porto-lume"), ""))
        .about("an entity with a picture, links, backlinks and a file")
        .node("memory", panel(|_| Page::id("remembered"), ""))
        .sized((560.0, 520.0))
        .about("the memory page: one dated line per thing kept, in every brief whole")
        .node("skill", panel(|_| Page::id("filing-a-source"), ""))
        .sized((560.0, 640.0))
        .about("a skill: use is first on the bar, before ask — a chat with *follow this skill* as its first turn")
        .node("dangling", panel(|_| Page::id("sailing-trip-2026"), ""))
        .about("[[tide-tables]] names no page: muted in the body, said so under links")
        .node("renaming", panel(|_| Page::id("joule-heating"), "click \"rename\"\nwait 500"))
        .sized((560.0, 520.0))
        .about("rename stands a field where the title is; enter rewrites every inbound link")
        .node("phone", phone_panel(|_| Page::id("porto-lume"), ""))
        .sized((380.0, 760.0))
        .about("a page on the cover display reads whole")
        .edge("entity", "renaming", "rename")
}

// -- the history -------------------------------------------------------------------

fn history() -> Scene<Setup> {
    Scene::new("kb history", (560.0, 520.0))
        .note("Every write of a page, newest first: the date, the device, the author — the editor, the import, or the chat by its title, with a link that opens it — and the message. A revision opens as a reading with restore on its bar.")
        .node("history", panel(|_| History::id("lamp-build"), ""))
        .about("four writes: the import, an edit, the agent's in *file the tax letter*, and an edit on the phone")
        .node("revision", panel(|_| Revision::id(&seed::restorable_revision()), ""))
        .sized((560.0, 640.0))
        .about("the document as the second write left it; restore files it as the page's next revision")
        .edge("history", "revision", "a row")
}

// -- the editor --------------------------------------------------------------------

fn edit() -> Scene<Setup> {
    Scene::new("kb edit", (600.0, 700.0))
        .note("A page's document in the shell's source editor: the frontmatter block over the body, the Markdown spans drawn, every change kept as a draft, save writing the page and its revision.")
        .node("page", panel(|_| Edit::id("lamp-build"), ""))
        .about("an existing page with its frontmatter")
        .node("new", panel(|_| Edit::new_id(), ""))
        .sized((600.0, 420.0))
        .about("a new page: a blank block for the frontmatter and nothing else; the first save takes the slug from the title")
        .node(
            "typed",
            panel(
                |_| Edit::new_id(),
                "click \"editor\"\nwait 300\nkey cmd+a\nwait 100\npaste \"---\\ntype: concept\\ntitle: Anchoring in a blow\\nsummary: What holds when the wind gets up.\\ntags: [sailing]\\n---\\n\\nSeven to one scope, and **more chain** than seems reasonable. See [[porto-lume]].\\n\"\nwait 600",
            ),
        )
        .about("a document typed in: the block dim, the heading bold, the link dim — and the draft line under it")
        .edge("new", "typed", "type")
}

// -- the file card -----------------------------------------------------------------

fn file() -> Scene<Setup> {
    let with_state = |path: &'static str, state: Where| {
        panel(
            move |store| {
                model::set_where(store, &hash_of(path), state);
                File::id(path)
            },
            "",
        )
    };
    Scene::new("kb file", (560.0, 760.0))
        .note("One file's card: the name, the kind and the size, a muted line saying where the bytes are, the pages that name it, and under a rule the shared viewer.")
        .note("The bytes here are the seed's own — a picture drawn by code, a one-page PDF written by code — standing in for what the blob cache will hand over in phase 3.")
        .node("pdf", panel(|_| File::id(seed::PDF), ""))
        .about("in the bucket · cached: the marina's price list under the viewer, its text selectable")
        .node("picture", panel(|_| File::id(seed::PICTURE), ""))
        .sized((560.0, 560.0))
        .about("a picture, cached: the harbour from the mole")
        .node("fetching", with_state(seed::PICTURE, Where::Fetching))
        .sized((560.0, 360.0))
        .about("fetching…: the bucket has been asked; the card redraws when the bytes land")
        .node("outbox", panel(|_| File::id(seed::TEXT), ""))
        .sized((560.0, 560.0))
        .about("in the outbox, not backed up yet: the parts list, here but not in any bucket")
        .node("not here", with_state(seed::PDF, Where::Missing))
        .sized((560.0, 360.0))
        .about("not here: fetch on the bar, and nothing under the rule")
        .edge("not here", "fetching", "fetch")
        .edge("fetching", "picture", "the bytes land")
}

// -- the import form ---------------------------------------------------------------

fn import() -> Scene<Setup> {
    Scene::new("kb import", (520.0, 380.0))
        .note("The folder the KB was kept in, and the one press that reads it in. browse opens the files picker for a folder; import reads; the status line says what it found and how many uploads are still to go.")
        .note("Nothing is read in phase 0: the line walks the three states on the world's clock.")
        .node("idle", panel(|_| Import::id(), ""))
        .about("~/cloud/KB, where the folder is on the Mac that has it")
        .node("reading", panel(|_| Import::id(), "click \"import\"\nwait 400"))
        .about("reading…")
        .node("done", panel(|_| Import::id(), "click \"import\"\nwait 2600"))
        .about("101 pages, 96 files · 12 of 96 uploaded — the uploads go on as jobs")
        .edge("idle", "reading", "import")
        .edge("reading", "done", "the read ends")
}
