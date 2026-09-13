//! Reading the course out loud: the effect behind **play**, and the tag it
//! asks the voice to read in.
//!
//! In memory and not a queued row. Nobody retries a sentence — by the time
//! a retry came round the learner would have turned the card — and nobody
//! waits for one either: [`Speech::speak`] answers that the voice took the
//! words, not that it reached the end of them. It goes through the one door
//! all the same, so a run that said nothing says why in the effects log.
//!
//! The voice is the shell's ([`platform::speech`](crate::platform::speech)),
//! installed for a real run nobody scripts. A scripted run, a library mount
//! and a test get the kernel's fake, which keeps every sentence instead of
//! saying it; a world with no voice at all refuses, and the panel offers
//! the words in writing.

use kernel::caps::Speech;
use kernel::effect::{Ctx, Effect};
use kernel::store::Store;

use super::model;

/// How much of a sentence the effects log shows before the ellipsis. A
/// card's words are a term and its example; a set piece's are a passage.
const DESCRIBED: usize = 60;

/// Say something, in a language.
pub struct Speak {
    pub text: String,
    /// A BCP-47 tag — `de-DE` — from the learner's target language.
    pub lang: String,
}

impl Effect for Speak {
    const KIND: &'static str = "fluent.speak";
    type Reply = ();

    fn describe(&self) -> String {
        format!("speak „{}“", line(&self.text))
    }

    /// Nothing of ours changes, and nothing out there is left changed
    /// either: the room is quiet again a moment later.
    fn writes(&self) -> bool {
        false
    }

    fn perform(&self, cx: &mut Ctx<'_>) -> Result<(), String> {
        cx.cap::<dyn Speech>()?.speak(&self.text, &self.lang)
    }
}

/// The tag this course's words are read in: the learner's target language,
/// or German where there is no learner row yet to ask.
#[must_use]
pub fn lang(store: &Store) -> &'static str {
    lang_of(&model::learner(store).map(|l| l.target).unwrap_or_default())
}

/// The tag for a language the course names.
///
/// The learner row spells its languages the way a person says them —
/// *German*, *Russian* — and a synthesizer wants BCP-47; between the two is
/// this list. A language nobody has named here is read as German, because
/// German is what the course is until a tutor says otherwise.
#[must_use]
pub fn lang_of(target: &str) -> &'static str {
    match target.trim().to_lowercase().as_str() {
        "german" => "de-DE",
        "russian" => "ru-RU",
        "english" => "en-US",
        "french" => "fr-FR",
        "spanish" => "es-ES",
        "italian" => "it-IT",
        _ => "de-DE",
    }
}

/// One line of it, short enough for a row: every run of whitespace is a
/// single space, and what is left is cut where it stops being readable.
fn line(text: &str) -> String {
    let one = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match one.char_indices().nth(DESCRIBED) {
        Some((at, _)) => format!("{}…", &one[..at]),
        None => one,
    }
}

#[cfg(test)]
mod tests {
    use kernel::app::App;
    use kernel::caps::FakeSpeech;
    use kernel::layout::SlotId;
    use kernel::panel::PanelId;
    use kernel::session::{Action, Session};

    use super::super::{Review, FLUENT};
    use super::*;

    static APPS: &[&dyn App] = &[&FLUENT];

    fn open(s: &mut Session, id: PanelId) -> SlotId {
        s.act(Action::new("open", "open").moving(move |wm| {
            wm.open(id, None, false);
        }));
        s.settle();
        s.focus().unwrap()
    }

    /// Every language the course can be set to, and the one anything else
    /// falls back to.
    #[test]
    fn each_language_the_course_names_is_read_in_its_own_tongue() {
        assert_eq!(lang_of("German"), "de-DE");
        assert_eq!(lang_of(" russian "), "ru-RU", "spelled as the row has it");
        assert_eq!(lang_of("English"), "en-US");
        assert_eq!(lang_of("French"), "fr-FR");
        assert_eq!(lang_of("Spanish"), "es-ES");
        assert_eq!(lang_of("Italian"), "it-IT");
        assert_eq!(lang_of("Klingon"), "de-DE", "and the course is German");
        assert_eq!(lang_of(""), "de-DE");
    }

    /// What the log says of a sentence: one line, cut where it stops being
    /// a row.
    #[test]
    fn a_spoken_line_describes_itself_in_one_line() {
        let short = Speak {
            text: "die Gebühr".into(),
            lang: "de-DE".into(),
        };
        assert_eq!(short.describe(), "speak „die Gebühr“");
        assert!(!short.writes(), "the room is quiet again after");

        let long = Speak {
            text: "die Gebühr.\nDie Anmeldung kostet eine Gebühr von dreißig Euro, \
                   und der Sachbearbeiter prüft den Antrag."
                .into(),
            lang: "de-DE".into(),
        };
        let said = long.describe();
        assert!(
            said.starts_with("speak „die Gebühr. Die Anmeldung"),
            "{said}"
        );
        assert!(said.ends_with("…“"), "{said}");
        assert!(said.chars().count() < 80, "{said}");
    }

    /// **play** on a card hands the card's own audio line to the world's
    /// voice, in the language the learner is learning.
    #[test]
    fn play_reads_the_card_aloud_in_the_target_language() {
        let mut s = Session::fake(APPS);
        let voice = s
            .world()
            .caps(|c| c.get::<FakeSpeech>().cloned())
            .expect("a fake world keeps a voice under its own type");
        let slot = open(&mut s, Review::id());

        let audio = {
            let panel = s.panel(slot).expect("the review panel");
            let mut borrow = panel.borrow_mut();
            let review = borrow.as_any().downcast_mut::<Review>().expect("a review");
            review.current().expect("a card is due").audio.clone()
        };
        assert!(!audio.trim().is_empty(), "the demo card has words to read");
        assert!(voice.spoken().is_empty(), "nothing said before the press");

        {
            let panel = s.panel(slot).expect("the review panel");
            let mut borrow = panel.borrow_mut();
            borrow.run("fluent.play", &mut s);
        }
        assert_eq!(voice.spoken(), vec![(audio, "de-DE".to_string())]);
    }
}
