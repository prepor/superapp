//! Panels for shell interaction tests, registered through the public kernel
//! interfaces so the tests do not depend on any bundled app.

use kernel::app::App;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag};

pub(super) fn panel(name: &str) -> PanelId {
    PanelId::new(Tag("shell-test"), [name])
}

struct TestPanel(PanelId);

impl Panel for TestPanel {
    fn id(&self) -> &PanelId {
        &self.0
    }

    fn title(&self) -> String {
        self.0.arg(0).unwrap().into()
    }

    fn about(&self) -> String {
        format!("Test context for {}", self.title())
    }

    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

struct TestKind;

impl PanelKind for TestKind {
    fn tag(&self) -> Tag {
        Tag("shell-test")
    }

    fn open(&self, id: &PanelId, _: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(TestPanel(id.clone()))
    }
}

pub(super) struct TestApp;
pub(super) static TEST_APP: TestApp = TestApp;

impl App for TestApp {
    fn id(&self) -> &'static str {
        "shell-test"
    }

    fn kinds(&self) -> &'static [&'static dyn PanelKind] {
        &[&TestKind]
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
