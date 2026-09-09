//! Panel-owned viewer state. Verbs and gestures drive the same surface.

use std::{cell::RefCell, collections::VecDeque, rc::Rc};
use kernel::panel::Verb;
use super::Measure;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command { ZoomIn, ZoomOut, Fit, FitWidth }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Fit { #[default] Page, Width }

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Status {
    pub ready: bool,
    pub page: usize,
    pub pages: usize,
    pub scale: f64,
    pub fit: Option<Fit>,
    pub selected: bool,
    pub text_pending: bool,
    pub text_error: Option<String>,
}

#[derive(Default)]
struct State {
    measure: Measure,
    status: Status,
    commands: VecDeque<Command>,
}

/// An app keeps this on its panel, binds its widget, and delegates verbs/run.
/// The viewer publishes measurements and reading state back through it.
#[derive(Clone, Default)]
pub struct Controller(Rc<RefCell<State>>);

impl Controller {
    pub fn measure(&self) -> Measure { self.0.borrow().measure.clone() }

    pub fn measured(&self, measure: Measure) -> bool {
        let mut state = self.0.borrow_mut();
        if state.measure == measure { return false; }
        state.measure = measure;
        true
    }

    pub fn verbs(&self) -> Vec<Verb> {
        let state = self.0.borrow();
        if !state.status.ready { return Vec::new(); }
        let mut verbs = vec![
            Verb::run("viewer.fit", "fit page", Some('f')),
            Verb::run("viewer.fit_width", "fit width", None),
            Verb::run("viewer.zoom_in", "zoom in", None),
            Verb::run("viewer.zoom_out", "zoom out", None),
        ];
        if state.status.pages == 0 { verbs[0].label = "fit image".into(); }
        verbs
    }

    /// The owner requests the usual session redraw when this returns true.
    pub fn run(&self, verb: &str) -> bool {
        let command = match verb {
            "viewer.zoom_in" => Command::ZoomIn,
            "viewer.zoom_out" => Command::ZoomOut,
            "viewer.fit" => Command::Fit,
            "viewer.fit_width" => Command::FitWidth,
            _ => return false,
        };
        self.0.borrow_mut().commands.push_back(command);
        true
    }

    pub(super) fn take(&self) -> Vec<Command> { self.0.borrow_mut().commands.drain(..).collect() }
    pub(super) fn status(&self, status: Status) -> bool {
        let mut state = self.0.borrow_mut();
        if state.status == status { return false; }
        state.status = status;
        true
    }
    pub(super) fn reset(&self) {
        let mut state = self.0.borrow_mut();
        state.status = Status::default();
        state.commands.clear();
    }
}
