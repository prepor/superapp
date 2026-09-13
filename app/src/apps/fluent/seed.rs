//! The demo course: a learner twelve days into a streak, a deck with ten
//! cards due, a lesson on the shelf for today, a lesson finished yesterday,
//! and the grammar the tutor has written up so far.
//!
//! Only a `Fake` world gets it — a real store starts empty, for the tutor
//! to fill.

#![allow(clippy::type_complexity)]

use kernel::app::Mode;
use kernel::store::Store;
use kernel::time::virtual_epoch;
use rusqlite::{params, Connection};

use super::model::{self, Closed};
use super::sm2::{self, day_start, State, DAY};

/// The lesson on the shelf for today, and the one finished yesterday.
pub const READY: i64 = 100;
pub const LAST_DONE: i64 = 99;

/// A seeded lesson's uid — the name it goes by on every device — is its id
/// spelled out, so a test can name one the way another device would.
#[must_use]
pub fn uid(lesson: i64) -> String {
    format!("seed-lesson-{lesson}")
}

/// The first card the review shows: the one longest overdue.
#[cfg(test)]
pub const FIRST_DUE: &str = "vocab_verlaengern";

pub fn seed(store: &Store, mode: Mode) -> rusqlite::Result<()> {
    // A real store starts empty; a fake or a denied world — a scripted run,
    // a library mount — gets the course.
    if mode == Mode::Real {
        return Ok(());
    }
    store.write(|c| {
        if c.query_row("SELECT COUNT(*) FROM fluent_learner", [], |r| r.get::<_, i64>(0))? > 0 {
            return Ok(());
        }
        let now = virtual_epoch();
        let today = day_start(now);
        let day = |n: i64| today + n as f64 * DAY;
        c.execute(
            "INSERT INTO fluent_learner(id, name, native, target, level, goal, daily_minutes, streak, last_active, started)
             VALUES(1, 'Andrey', 'Russian', 'German', 'A2', 'B1', 30, 12, ?1, ?2)",
            params![day(-1) + 18.4 * 3600.0, day(-140)],
        )?;
        items(c, &day)?;
        cards(c)?;
        topics(c, &day)?;
        mistakes(c, &day)?;
        skills(c, &day)?;
        lessons(c, &day)?;
        played(c)?;
        // The schedule is not seeded, it is replayed: every item now
        // stands where the grades above put it.
        model::recompute_all_tx(c)?;
        Ok(())
    })
}

/// The grades yesterday's lesson filed as it was played: one per item each
/// answered exercise names, stamped when that exercise was answered, under
/// the lesson's uid and the exercise's seq — the way the player files them.
/// A self-check answer files the tutor's quality where the tutor has
/// spoken, because the tutor's word is the last one on the schedule.
fn played(c: &Connection) -> rusqlite::Result<()> {
    let mut stmt = c.prepare(
        "SELECT seq, items, result, self_grade, tutor_grade, answered FROM fluent_exercise
          WHERE lesson_uid = ?1 AND answered IS NOT NULL ORDER BY seq",
    )?;
    let answered: Vec<(i64, String, Option<String>, Option<i64>, Option<i64>, f64)> = stmt
        .query_map([uid(LAST_DONE)], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    for (seq, items, result, self_grade, tutor_grade, at) in answered {
        let quality = match result.as_deref() {
            Some("correct") => Some(Closed::Correct.quality()),
            Some("almost") => Some(Closed::Almost.quality()),
            Some("wrong") => Some(Closed::Wrong.quality()),
            _ => tutor_grade.or(self_grade),
        };
        let Some(quality) = quality else { continue };
        for (n, item) in model::json_strings(&items).iter().enumerate() {
            c.execute(
                "INSERT INTO fluent_review(item, at, quality, device, lesson_uid, seq)
                 VALUES(?1, ?2, ?3, 'desk', ?4, ?5)",
                params![item, at + n as f64 * 0.001, quality, uid(LAST_DONE), seq],
            )?;
        }
    }
    Ok(())
}

/// The grades one item was given, as `(days ago, quality)` pairs: the
/// qualities in order, walked back from the day the item comes due, each
/// one given on the day the grade before it made the item due. That is
/// what a kept schedule looks like, and replaying it lands the item
/// exactly where its row says it stands.
fn history(due: i64, qualities: &[i64]) -> Vec<(i64, i64)> {
    let mut state = State::default();
    let intervals: Vec<i64> = qualities
        .iter()
        .map(|q| {
            state = sm2::step(state, *q);
            state.interval
        })
        .collect();
    let mut days = vec![0_i64; qualities.len()];
    let mut when = due;
    for (day, interval) in days.iter_mut().zip(&intervals).rev() {
        when -= interval;
        *day = when;
    }
    days.into_iter().zip(qualities.iter().copied()).collect()
}

/// `(id, kind, content, created days ago, due in days, grades)`.
///
/// The grades are the qualities the item was given, oldest first; the days
/// they were given on come from [`history`], walked back from the day the
/// item is due here. So the schedule is never written: it is what replaying
/// these leaves. An item with no grades is fresh — a word yesterday's
/// lesson introduced — and comes due on the day the row says.
///
/// Yesterday's lesson files its own grades over the top of these, which is
/// why the items it names come due on its day: it took them when they were
/// due, and where they go next is its grade's business.
fn items(c: &Connection, day: &dyn Fn(i64) -> f64) -> rusqlite::Result<()> {
    let rows: &[(&str, &str, &str, i64, i64, &[i64])] = &[
        ("vocab_die_gebuehr", "vocab", "die Gebühr", 20, -1, &[3, 4, 2, 4]),
        ("vocab_der_termin", "vocab", "der Termin", 60, 0, &[5, 5, 5]),
        ("vocab_die_aufenthaltserlaubnis", "vocab", "die Aufenthaltserlaubnis", 12, 0, &[3, 2, 3]),
        ("vocab_der_antrag", "vocab", "der Antrag", 40, 0, &[4, 5]),
        ("vocab_die_ueberweisung", "vocab", "die Überweisung", 3, 0, &[4]),
        ("vocab_das_passfoto", "vocab", "das Passfoto", 9, 0, &[3, 4]),
        ("vocab_verlaengern", "vocab", "verlängern", 14, -2, &[3, 4]),
        ("vocab_der_nachweis", "vocab", "der Nachweis", 8, 1, &[4, 5]),
        ("vocab_das_einkommen", "vocab", "das Einkommen", 6, 2, &[5, 5]),
        ("vocab_die_bestaetigung", "vocab", "die Bestätigung", 10, 3, &[4, 5]),
        ("vocab_der_sachbearbeiter", "vocab", "der Sachbearbeiter", 9, 4, &[3, 4]),
        ("vocab_abgeben", "vocab", "abgeben", 25, 5, &[5, 5, 5]),
        ("vocab_ausfuellen", "vocab", "ausfüllen", 35, 6, &[5, 5, 5]),
        ("vocab_die_frist", "vocab", "die Frist", 22, -1, &[4, 5]),
        ("vocab_die_gueltigkeit", "vocab", "die Gültigkeit", 20, 10, &[3, 4, 4]),
        ("vocab_unterschreiben", "vocab", "unterschreiben", 30, 12, &[5, 5, 4]),
        ("vocab_die_unterschrift", "vocab", "die Unterschrift", 60, 15, &[4, 4, 5, 5]),
        ("vocab_das_formular", "vocab", "das Formular", 70, 20, &[5, 5, 5, 5]),
        ("vocab_der_briefkasten", "vocab", "der Briefkasten", 80, 24, &[5, 5, 5, 4]),
        ("vocab_die_behoerde", "vocab", "die Behörde", 75, 30, &[4, 5, 5, 5]),
        ("vocab_der_vermieter", "vocab", "der Vermieter", 1, 0, &[]),
        ("vocab_die_kaution", "vocab", "die Kaution", 1, 0, &[]),
        ("vocab_die_nebenkosten", "vocab", "die Nebenkosten", 1, 0, &[]),
        ("vocab_die_einbuergerung", "vocab", "die Einbürgerung", 120, 40, &[5, 5, 5, 5]),
        ("article_gender", "grammar", "der/die/das nach Fall", 130, -1, &[4, 5]),
        ("v2_word_order", "grammar", "Verb an Position 2", 110, -1, &[2, 3, 4]),
        ("dativ_prepositions", "grammar", "aus bei mit nach seit von zu + Dativ", 40, -1, &[4]),
        ("wechselpraepositionen", "grammar", "Wo? Dativ — Wohin? Akkusativ", 50, 2, &[4, 5]),
        ("adjektiv_endungen", "grammar", "Adjektivendungen", 15, 0, &[2, 3]),
        ("verb_sein", "grammar", "sein: bin bist ist sind seid", 140, 20, &[5, 5, 5, 5]),
        ("numbers_1_to_9", "grammar", "Zahlen 1–9 als Wörter", 140, 15, &[5, 5, 5, 4]),
        ("listening_times", "grammar", "Uhrzeiten hören", 30, 4, &[3, 4, 4]),
        ("writing_official", "grammar", "kurze Sätze im Amt", 20, -1, &[3]),
        ("indefinitpronomen_man", "grammar", "man als Subjekt", 25, 7, &[4, 5, 5]),
        ("eszett_usage", "error", "ß vs ss: heiße, not heisse", 120, 6, &[4, 5, 5]),
        ("spurious_subject_die_man", "error", "»Die man …« — man is the subject", 25, 3, &[3, 4]),
        ("dativ_after_mit", "error", "mit + Dativ: mit seiner Tochter", 20, 1, &[2, 4, 5]),
        ("article_gender_fem", "error", "feminine nouns take die", 20, -1, &[4]),
    ];
    for (n, (id, kind, content, ago, due, grades)) in rows.iter().enumerate() {
        c.execute(
            "INSERT INTO fluent_item(id, kind, content, created, due) VALUES(?1, ?2, ?3, ?4, ?5)",
            params![id, kind, content, day(-ago), day(*due)],
        )?;
        // Where a grade was given, alternating so a card's history shows
        // both: the deck is turned on the phone, a lesson is played here.
        let device = if n % 2 == 0 { "phone" } else { "desk" };
        for (when, quality) in history(*due, grades) {
            c.execute(
                "INSERT INTO fluent_review(item, at, quality, device) VALUES(?1, ?2, ?3, ?4)",
                params![id, day(when) + 19.0 * 3600.0, quality, device],
            )?;
        }
    }
    Ok(())
}

fn cards(c: &Connection) -> rusqlite::Result<()> {
    let rows: &[(&str, &str, &str, &str, &str)] = &[
        ("vocab_die_gebuehr", "die Gebühr", "сбор, пошлина / a fee", "Die Anmeldung kostet eine Gebühr von 30 Euro.", "Feminin: die Gebühr, -en. Von Gebühren spricht das Amt, von Preisen das Geschäft."),
        ("vocab_der_termin", "der Termin", "встреча по записи / an appointment", "Ich habe morgen einen Termin bei der Behörde.", "Maskulin: der Termin, -e. Einen Termin vereinbaren, absagen, verschieben."),
        ("vocab_die_aufenthaltserlaubnis", "die Aufenthaltserlaubnis", "вид на жительство / a residence permit", "Ein Ausländer braucht eine Aufenthaltserlaubnis, um legal in Deutschland zu leben.", "Feminin. Kompositum: der Aufenthalt + die Erlaubnis — das Grundwort bestimmt das Genus."),
        ("vocab_der_antrag", "der Antrag", "заявление / an application", "Ich stelle einen Antrag auf Verlängerung.", "Maskulin: der Antrag, die Anträge. Einen Antrag stellen, abgeben, ausfüllen."),
        ("vocab_die_ueberweisung", "die Überweisung", "банковский перевод / a bank transfer", "Ich bezahle die Gebühr per Überweisung.", "Feminin. Von überweisen (переводить деньги)."),
        ("vocab_das_passfoto", "das Passfoto", "фото на паспорт / a passport photo", "Bringen Sie bitte ein aktuelles Passfoto mit.", "Neutrum: das Passfoto, -s. Biometrisch, nicht älter als sechs Monate."),
        ("vocab_verlaengern", "verlängern", "продлевать / to extend", "Ich möchte meinen Aufenthaltstitel verlängern.", "Regelmäßig: verlängerte, hat verlängert. Das Nomen: die Verlängerung."),
        ("vocab_der_nachweis", "der Nachweis", "подтверждение, справка / a proof", "Für die Verlängerung brauche ich einen Nachweis über mein Einkommen.", "Maskulin: der Nachweis, -e. Oft als Kompositum: Einkommensnachweis, Krankenversicherungsnachweis."),
        ("vocab_das_einkommen", "das Einkommen", "доход / an income", "Mein Einkommen reicht für die Miete.", "Neutrum. Ein Einkommensnachweis zeigt, wie viel Geld du verdienst."),
        ("vocab_die_bestaetigung", "die Bestätigung", "подтверждение / a confirmation", "Wir schicken Ihnen eine Bestätigung per Post.", "Feminin. Nicht verwechseln mit die Bescheinigung (справка): eine Bestätigung bestätigt, dass etwas erledigt ist."),
        ("vocab_der_sachbearbeiter", "der Sachbearbeiter", "сотрудник, ведущий дело / a case officer", "Der Sachbearbeiter prüft meinen Antrag.", "Maskulin; die Sachbearbeiterin. Die Person im Amt, die deinen Fall bearbeitet."),
        ("vocab_abgeben", "abgeben", "сдать / to hand in", "Ich gebe den Antrag am Montag ab.", "Trennbar: gibt ab, gab ab, hat abgegeben."),
        ("vocab_ausfuellen", "ausfüllen", "заполнять / to fill in", "Bitte füllen Sie das Formular vollständig aus.", "Trennbar: füllt aus, hat ausgefüllt."),
        ("vocab_die_frist", "die Frist", "срок / a deadline", "Die Frist endet am 30. September.", "Feminin: die Frist, -en. Eine Frist einhalten, verpassen, verlängern."),
        ("vocab_die_gueltigkeit", "die Gültigkeit", "срок действия / validity", "Die Gültigkeit des Passes endet nächstes Jahr.", "Feminin. Von gültig (действительный). Nicht die Frist — das ist ein Termin, kein Zeitraum."),
        ("vocab_unterschreiben", "unterschreiben", "подписать / to sign", "Bitte unterschreiben Sie hier.", "Untrennbar: unterschreibt, hat unterschrieben — kein ge-."),
        ("vocab_die_unterschrift", "die Unterschrift", "подпись / a signature", "Ohne Unterschrift ist der Antrag nicht gültig.", "Feminin: die Unterschrift, -en."),
        ("vocab_das_formular", "das Formular", "бланк / a form", "Das Formular gibt es auch online.", "Neutrum: das Formular, -e."),
        ("vocab_der_briefkasten", "der Briefkasten", "почтовый ящик / a letterbox", "Der Bescheid liegt im Briefkasten.", "Maskulin: der Briefkasten, die Briefkästen."),
        ("vocab_die_behoerde", "die Behörde", "ведомство / an authority", "Die Behörde hat montags geschlossen.", "Feminin: die Behörde, -n. Ausländerbehörde, Meldebehörde."),
        ("vocab_der_vermieter", "der Vermieter", "арендодатель / a landlord", "Der Vermieter will eine Kaution.", "Maskulin; die Vermieterin. Von vermieten (сдавать в аренду)."),
        ("vocab_die_kaution", "die Kaution", "залог / a deposit", "Die Kaution beträgt zwei Monatsmieten.", "Feminin: die Kaution, -en."),
        ("vocab_die_nebenkosten", "die Nebenkosten", "коммунальные платежи / utilities", "Die Nebenkosten sind in der Miete nicht enthalten.", "Nur Plural. Kaltmiete + Nebenkosten = Warmmiete."),
        ("vocab_die_einbuergerung", "die Einbürgerung", "натурализация / naturalization", "Für die Einbürgerung brauche ich B1.", "Feminin. Von einbürgern. Der Einbürgerungstest hat 33 Fragen."),
    ];
    for (item, front, back, example, notes) in rows {
        c.execute(
            "INSERT INTO fluent_card(item, front, back, example, audio, notes) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![item, front, back, example, format!("{front}. {example}"), notes],
        )?;
    }
    Ok(())
}

fn topics(c: &Connection, day: &dyn Fn(i64) -> f64) -> rusqlite::Result<()> {
    let rows: &[(&str, &str, &str, &str, &str, Option<i64>, &str, Option<i64>, Option<i64>, &str, &str)] = &[
        (
            "artikel-nom-akk-dat",
            "Artikel: Nominativ, Akkusativ, Dativ",
            "cases",
            "A1",
            "Wie der/die/das und ein/eine sich nach Fall verändern.",
            Some(3),
            r#"["article_gender","article_gender_fem"]"#,
            Some(3),
            Some(LAST_DONE),
            r#"[{"kind":"text","body":"Der Artikel zeigt Genus und Fall zugleich. Im Nominativ steht das Subjekt, im Akkusativ das direkte Objekt (wen? was?), im Dativ das indirekte Objekt (wem?) und alles nach aus, bei, mit, nach, seit, von, zu."},{"kind":"table","caption":"Bestimmte Artikel","columns":["","Maskulin","Feminin","Neutrum","Plural"],"rows":[["Nominativ","der","die","das","die"],["Akkusativ","den","die","das","die"],["Dativ","dem","der","dem","den + -n"]]},{"kind":"table","caption":"Unbestimmte Artikel","columns":["","Maskulin","Feminin","Neutrum"],"rows":[["Nominativ","ein","eine","ein"],["Akkusativ","einen","eine","ein"],["Dativ","einem","einer","einem"]]},{"kind":"examples","items":[{"text":"Ich sehe den Mann.","note":"Akkusativ — direktes Objekt"},{"text":"Ich gebe dem Mann den Brief.","note":"Dativ — wem? · Akkusativ — was?"},{"text":"Ich habe einen Balkon.","note":"haben verlangt Akkusativ: ein → einen"}]},{"kind":"tip","body":"Nur Maskulin ändert sich im Akkusativ: der → den, ein → einen. Alles andere bleibt wie im Nominativ."}]"#,
            r#"["wechselpraepositionen","dativ-praepositionen"]"#,
        ),
        (
            "v2-wortstellung",
            "V2-Wortstellung: das Verb steht an zweiter Stelle",
            "sentence_structure",
            "A2",
            "Im Hauptsatz steht das konjugierte Verb immer an Position 2 — egal, was an Position 1 steht.",
            Some(3),
            r#"["v2_word_order"]"#,
            Some(5),
            Some(24),
            r#"[{"kind":"text","body":"Position 1 kann das Subjekt sein, eine Zeitangabe, ein Objekt — aber Position 2 gehört immer dem konjugierten Verb. Kommt etwas anderes an Position 1, rückt das Subjekt hinter das Verb (Inversion). Ein zweiter Verbteil — Infinitiv, Partizip, trennbare Vorsilbe — steht am Satzende."},{"kind":"table","caption":"Vier Sätze, ein Verb an Position 2","columns":["Position 1","Verb","Mitte","Ende"],"rows":[["Ich","fülle","den Antrag","aus."],["Morgen","fülle","ich den Antrag","aus."],["Den Antrag","fülle","ich morgen","aus."],["Ich","muss","den Antrag","ausfüllen."]]},{"kind":"examples","items":[{"text":"Am Montag gebe ich den Antrag ab.","note":"Zeitangabe vorn ⇒ Subjekt nach dem Verb"},{"text":"Ich muss eine Gebühr bezahlen.","note":"Modalverb an 2, Infinitiv am Ende"}]},{"kind":"tip","body":"Zähle nicht Wörter, zähle Satzglieder: »Am Montag« ist eine Position."}]"#,
            r#"["indefinitpronomen-man"]"#,
        ),
        (
            "dativ-praepositionen",
            "Dativ-Präpositionen: aus, bei, mit, nach, seit, von, zu",
            "prepositions",
            "B1",
            "Sieben Präpositionen, die immer den Dativ verlangen.",
            Some(2),
            r#"["dativ_prepositions","dativ_after_mit"]"#,
            Some(9),
            Some(21),
            r#"[{"kind":"text","body":"Nach diesen sieben Präpositionen steht der Dativ, immer — egal ob Ort, Zeit oder Richtung gemeint ist."},{"kind":"table","caption":"Mit bestimmtem Artikel","columns":["","Maskulin","Feminin","Neutrum","Plural"],"rows":[["mit","dem Sachbearbeiter","der Behörde","dem Amt","den Ämtern"],["zu","dem Termin (zum)","der Behörde (zur)","dem Amt (zum)","den Terminen"],["von","dem Vermieter (vom)","der Frau","dem Kind","den Kindern"]]},{"kind":"examples","items":[{"text":"Ich gehe mit meiner Tochter zum Amt.","note":"mit + Dativ, zu + dem = zum"},{"text":"Seit einem Jahr wohne ich in Berlin.","note":"seit + Dativ, auch bei Zeit"}]},{"kind":"tip","body":"Merksatz: »aus bei mit nach seit von zu — immer mit dem Dativ du.«"}]"#,
            r#"["wechselpraepositionen","artikel-nom-akk-dat"]"#,
        ),
        (
            "wechselpraepositionen",
            "Wechselpräpositionen: Wo? Dativ — Wohin? Akkusativ",
            "prepositions",
            "B1",
            "Neun Präpositionen, die je nach Frage Dativ oder Akkusativ verlangen.",
            Some(2),
            r#"["wechselpraepositionen"]"#,
            Some(11),
            Some(22),
            r#"[{"kind":"text","body":"an, auf, hinter, in, neben, über, unter, vor, zwischen: Steht etwas irgendwo (Wo?), folgt der Dativ. Bewegt sich etwas irgendwohin (Wohin?), folgt der Akkusativ."},{"kind":"table","caption":"Wo? / Wohin?","columns":["Frage","Fall","Beispiel"],"rows":[["Wo?","Dativ","Ich bin in der Behörde."],["Wohin?","Akkusativ","Ich gehe in die Behörde."],["Wo?","Dativ","Der Brief liegt auf dem Tisch."],["Wohin?","Akkusativ","Ich lege den Brief auf den Tisch."]]},{"kind":"tip","body":"Frag dich: bleibt es (wo) oder geht es (wohin)? in + dem = im, an + dem = am, in + das = ins."}]"#,
            r#"["dativ-praepositionen"]"#,
        ),
        (
            "adjektiv-endungen",
            "Adjektivendungen nach bestimmtem Artikel",
            "adjectives",
            "B1",
            "Nach der/die/das endet das Adjektiv auf -e oder -en.",
            Some(1),
            r#"["adjektiv_endungen"]"#,
            Some(14),
            Some(25),
            r#"[{"kind":"text","body":"Steht ein bestimmter Artikel davor, trägt er die Information über Genus und Fall — das Adjektiv bekommt nur noch eine schwache Endung: -e im Nominativ Singular (und Akkusativ Feminin/Neutrum), sonst -en."},{"kind":"table","caption":"Schwache Endungen","columns":["","Maskulin","Feminin","Neutrum","Plural"],"rows":[["Nominativ","der neue Antrag","die neue Frist","das neue Formular","die neuen Fristen"],["Akkusativ","den neuen Antrag","die neue Frist","das neue Formular","die neuen Fristen"],["Dativ","dem neuen Antrag","der neuen Frist","dem neuen Formular","den neuen Fristen"]]},{"kind":"examples","items":[{"text":"Ich brauche das aktuelle Passfoto.","note":"Neutrum Akkusativ ⇒ -e"},{"text":"Mit dem aktuellen Passfoto ist der Antrag vollständig.","note":"Dativ ⇒ -en"}]}]"#,
            r#"["artikel-nom-akk-dat"]"#,
        ),
        (
            "indefinitpronomen-man",
            "Das Indefinitpronomen »man«: ein Subjekt reicht",
            "sentence_structure",
            "A2",
            "»man« ist ein unpersönliches Subjekt (люди вообще). Es steht allein an der Subjektstelle.",
            Some(2),
            r#"["indefinitpronomen_man","spurious_subject_die_man"]"#,
            Some(23),
            Some(15),
            r#"[{"kind":"text","body":"»man« bedeutet »люди вообще« und ist selbst das Subjekt. Es füllt die Subjektstelle komplett aus — kein weiteres Wort wie »die« oder »es« davor. Das Verb steht in der 3. Person Singular: man braucht, man muss, man bezahlt."},{"kind":"table","caption":"»man« an Position 1 oder nach einer anderen Angabe","columns":["Position 1","Verb","Subjekt","Rest"],"rows":[["Man","braucht","—","einen Reisepass."],["Für die Anmeldung","braucht","man","einen Reisepass."],["Hier","bezahlt","man","bar."]]},{"kind":"examples","items":[{"text":"Man braucht einen Reisepass und ein Formular.","note":"»man« allein — kein »Die man …«"},{"text":"Für die Anmeldung braucht man zwei Dokumente.","note":"Angabe an Position 1 ⇒ Verb an 2, »man« danach"}]},{"kind":"tip","body":"»man« ist schon das Subjekt — setz nie »die«, »es« oder ein anderes Subjekt davor."}]"#,
            r#"["v2-wortstellung"]"#,
        ),
    ];
    for (id, title, category, level, summary, mastery, items, introduced, practiced, sections, related) in rows {
        c.execute(
            "INSERT INTO fluent_topic(id, title, category, level, summary, mastery, items, introduced, practiced, sections, related, updated)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![id, title, category, level, summary, mastery, items, introduced, practiced, sections, related, day(-1)],
        )?;
    }
    // Each stumble names the lesson it came from the way every device
    // does, by uid; the local id the panel prints is the trigger's.
    let notes: &[(&str, i64, &str, i64)] = &[
        ("artikel-nom-akk-dat", LAST_DONE, "»ein Balkon« → einen Balkon — haben verlangt Akkusativ, maskulin → -en.", -1),
        ("artikel-nom-akk-dat", 24, "»der Unterschrift« → die Unterschrift — feminin, wie fast alle Nomen auf -schrift.", -3),
        ("indefinitpronomen-man", 15, "»Die man braucht einen Reisepass« → Man braucht einen Reisepass. »man« ist selbst das Subjekt — kein »die« davor.", -12),
        ("dativ-praepositionen", 21, "»mit seine Tochter« → mit seiner Tochter — mit verlangt Dativ.", -5),
    ];
    for (topic, lesson, note, d) in notes {
        c.execute(
            "INSERT INTO fluent_topic_note(topic, lesson_uid, note, at) VALUES(?1, ?2, ?3, ?4)",
            params![topic, uid(*lesson), note, day(*d) + 19.0 * 3600.0],
        )?;
    }
    Ok(())
}

fn mistakes(c: &Connection, day: &dyn Fn(i64) -> f64) -> rusqlite::Result<()> {
    let rows: &[(&str, &str, &str, i64, i64, &str, &str, &str, &str)] = &[
        ("article_gender", "grammar", "cases", 7, -1, "der Unterschrift", "die Unterschrift", "Übersetze ins Deutsche: подпись", "Feminine nouns keep slipping to der. Drill articles with the noun, never bare."),
        ("v2_word_order", "grammar", "sentence_structure", 4, -3, "Ich muss füllen einen Antrag aus", "Ich muss einen Antrag ausfüllen", "Free write: was brauchst du im Amt?", "With a modal the infinitive goes last; fine in drills, slips under time."),
        ("eszett_usage", "spelling", "special_characters", 3, -12, "heisse", "heiße", "Ich heiße Andrey", "Mastered in drills; reappears when typing fast."),
        ("dativ_after_mit", "grammar", "prepositions", 2, -5, "mit seine Tochter", "mit seiner Tochter", "Übersetze: он идёт с дочерью", "mit is a Dativ preposition; possessive gets -er in feminine Dativ."),
    ];
    for (id, category, sub, frequency, last, wrong, right, context, notes) in rows {
        c.execute(
            "INSERT INTO fluent_mistake(id, category, subcategory, frequency, last, notes, wrong, right, context)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![id, category, sub, frequency, day(*last), notes, wrong, right, context],
        )?;
    }
    Ok(())
}

fn skills(c: &Connection, day: &dyn Fn(i64) -> f64) -> rusqlite::Result<()> {
    for (name, mastery, accuracy, lessons, last) in [
        ("vocabulary", 5, 0.91, 31, -1),
        ("reading", 5, 0.94, 20, -1),
        ("writing", 4, 0.85, 28, -1),
        ("listening", 3, 0.88, 12, -2),
        ("speaking", 2, 0.70, 6, -20),
    ] {
        c.execute(
            "INSERT INTO fluent_skill(name, mastery, accuracy, lessons, practiced) VALUES(?1, ?2, ?3, ?4, ?5)",
            params![name, mastery, accuracy, lessons, day(last)],
        )?;
    }
    Ok(())
}

/// The lessons: a streak of twelve, a sparser eight weeks before it, the
/// one finished yesterday with its exercises, and today's on the shelf.
fn lessons(c: &Connection, day: &dyn Fn(i64) -> f64) -> rusqlite::Result<()> {
    const TITLES: &[&str] = &[
        "Zahlen und Uhrzeiten",
        "Im Supermarkt: Mengen und Preise",
        "Beim Bürgeramt: Anmeldung",
        "Der Weg zum Bahnhof",
        "Wetter und Jahreszeiten",
        "Familie und Beruf",
        "In der Apotheke",
        "Post und Pakete",
        "Kleidung kaufen",
        "Im Restaurant",
        "Fahrkarten und Verspätungen",
        "Ein Formular ausfüllen",
        "Das Wochenende erzählen",
        "Am Telefon: einen Termin vereinbaren",
        "Trennbare Verben im Alltag",
        "Nebensätze mit weil und dass",
        "Ausreden und Absagen",
        "Im Café: bestellen und bezahlen",
        "Beim Arzt: Termine und Beschwerden",
    ];
    // Days ago, oldest first: sparse for six weeks, then every day.
    const DAYS: &[i64] = &[54, 50, 45, 41, 38, 33, 30, 27, 24, 21, 20, 17, 15, 14, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2];
    let accuracy = [0.78, 0.82, 0.75, 0.86, 0.79, 0.88, 0.83, 0.91, 0.87, 0.92, 0.8, 0.9, 0.85, 0.94, 0.88, 0.83, 0.92, 0.89, 0.79, 0.92, 0.96, 0.85, 0.91, 0.88, 0.92];
    let minutes = [14.0, 18.0, 12.0, 21.0, 16.0, 24.0, 19.0, 22.0, 17.0, 26.0, 15.0, 20.0, 23.0, 19.0, 21.0, 18.0, 25.0, 22.0, 16.0, 24.0, 28.0, 19.0, 23.0, 21.0, 20.0];
    for (i, ago) in DAYS.iter().enumerate() {
        let title = TITLES[i % TITLES.len()];
        let start = day(-ago) + 18.0 * 3600.0;
        c.execute(
            "INSERT INTO fluent_lesson(id, uid, title, for_date, focus, status, generated, started, ended, accuracy, minutes, notes)
             VALUES(?1, ?2, ?3, ?4, '[\"everyday_conversation\"]', 'done', ?5, ?6, ?7, ?8, ?9, '')",
            params![
                i as i64 + 1,
                uid(i as i64 + 1),
                title,
                day(-ago),
                day(-ago - 1) + 22.0 * 3600.0,
                start,
                start + minutes[i] * 60.0,
                accuracy[i],
                minutes[i]
            ],
        )?;
    }

    // Yesterday's, whole.
    let start = day(-1) + 18.0 * 3600.0 + 120.0;
    c.execute(
        "INSERT INTO fluent_lesson(id, uid, title, for_date, focus, status, generated, started, ended, accuracy, minutes, notes)
         VALUES(?1, ?2, 'Wohnung suchen: Anzeigen lesen', ?3, '[\"housing\",\"article_gender\",\"reading\"]', 'done', ?4, ?5, ?6, 0.75, 22, ?7)",
        params![
            LAST_DONE,
            uid(LAST_DONE),
            day(-1),
            day(-2) + 22.0 * 3600.0,
            start,
            start + 22.0 * 60.0,
            "Solid on the reading; both slips were articles — die Kaution, einen Balkon. Tomorrow leans on Akkusativ after haben, and the three new housing words go on the deck."
        ],
    )?;
    let passage = "2-Zimmer-Wohnung, 54 m², Altbau, 3. OG ohne Aufzug. Kaltmiete 780 €, Nebenkosten 190 €, Kaution zwei Monatsmieten. Frei ab 1. Oktober. Haustiere nach Absprache.";
    let done: &[Ex] = &[
        Ex { seq: 1, section: "warmup", kind: "mcq", grading: "closed", prompt: "Welcher Artikel? ___ Wohnung", choices: &["die", "der", "das", "den"], accepted: &["die"], items: &["article_gender_fem"], answer: Some("die"), result: Some("correct"), ..Ex::DEFAULT },
        Ex { seq: 2, section: "warmup", kind: "cloze", grading: "closed", prompt: "Die Miete ohne Nebenkosten heißt ___miete.", accepted: &["Kalt"], explanation: "Kaltmiete + Nebenkosten = Warmmiete.", items: &["vocab_die_nebenkosten"], answer: Some("Kalt"), result: Some("correct"), ..Ex::DEFAULT },
        Ex { seq: 3, section: "review", kind: "translate", grading: "closed", prompt: "Übersetze ins Deutsche (mit Artikel): „залог“", accepted: &["die Kaution"], model: "die Kaution", explanation: "die Kaution — feminin, wie die meisten Nomen auf -ion.", items: &["vocab_die_kaution", "article_gender_fem"], answer: Some("der Kaution"), result: Some("wrong"), ..Ex::DEFAULT },
        Ex { seq: 4, section: "review", kind: "mcq", grading: "closed", prompt: "Was sind Nebenkosten?", choices: &["Heizung, Wasser, Müll", "die Miete ohne Heizung", "die Kaution", "der Vermieter"], accepted: &["Heizung, Wasser, Müll"], items: &["vocab_die_nebenkosten"], answer: Some("Heizung, Wasser, Müll"), result: Some("correct"), ..Ex::DEFAULT },
        Ex { seq: 5, section: "new", kind: "cloze", grading: "closed", prompt: "Ich suche eine Wohnung ___ Balkon.", accepted: &["mit"], explanation: "mit + Dativ: mit Balkon, mit zwei Zimmern.", items: &["dativ_prepositions"], answer: Some("mit"), result: Some("correct"), ..Ex::DEFAULT },
        Ex { seq: 6, section: "set_piece", kind: "read_mcq", grading: "closed", prompt: "Wie hoch ist die Warmmiete?", passage, choices: &["970 €", "780 €", "190 €", "1560 €"], accepted: &["970 €"], explanation: "Warmmiete = Kaltmiete 780 € + Nebenkosten 190 €.", items: &["vocab_die_nebenkosten"], answer: Some("780 €"), result: Some("wrong"), ..Ex::DEFAULT },
        Ex { seq: 7, section: "set_piece", kind: "read_mcq", grading: "closed", prompt: "Ab wann ist die Wohnung frei?", passage, choices: &["ab 1. Oktober", "ab sofort", "ab 3. Oktober", "nach Absprache"], accepted: &["ab 1. Oktober"], items: &["vocab_die_frist"], answer: Some("ab 1. Oktober"), result: Some("correct"), ..Ex::DEFAULT },
        Ex { seq: 8, section: "cooldown", kind: "free_write", grading: "self_check", prompt: "Schreib zwei Sätze über deine Traumwohnung.", model: "Meine Traumwohnung hat zwei Zimmer und einen Balkon. Sie liegt im Zentrum, nicht weit von der Arbeit.", items: &["writing_official", "article_gender"], answer: Some("Meine Traumwohnung hat zwei Zimmer und ein Balkon. Sie ist in Zentrum."), self_grade: Some(4), tutor_grade: Some(3), tutor_note: "Zwei Artikelfehler: einen Balkon (Akkusativ, maskulin), im Zentrum (in + Dativ, in dem = im). Der Rest ist klar und richtig.", tutor_fix: "Meine Traumwohnung hat zwei Zimmer und einen Balkon. Sie ist im Zentrum.", ..Ex::DEFAULT },
    ];
    for (n, ex) in done.iter().enumerate() {
        ex.insert(c, &uid(LAST_DONE), Some(start + 150.0 * (n as f64 + 1.0)))?;
    }

    // Today's, on the shelf.
    c.execute(
        "INSERT INTO fluent_lesson(id, uid, title, for_date, focus, status, generated)
         VALUES(?1, ?2, 'Bei der Ausländerbehörde: verlängern, bezahlen, bestätigen', ?3, '[\"official_documents\",\"article_gender\",\"vocab:behörde\"]', 'ready', ?4)",
        params![READY, uid(READY), day(0), day(-1) + 18.5 * 3600.0],
    )?;
    let letter = "Sehr geehrter Herr Iwanow, Ihr Aufenthaltstitel läuft am 30. September ab. Bitte vereinbaren Sie einen Termin zur Verlängerung. Bringen Sie Ihren Reisepass, ein aktuelles Passfoto und einen Nachweis über Ihr Einkommen mit. Die Gebühr beträgt 93 Euro und ist per Überweisung zu bezahlen.";
    let ready: &[Ex] = &[
        Ex { seq: 1, section: "warmup", kind: "mcq", grading: "closed", prompt: "Wähle die richtige Form von „sein“: „Ich ___ müde.“", choices: &["bin", "bist", "ist", "sind"], accepted: &["bin"], explanation: "ich bin, du bist, er/sie/es ist, wir sind.", items: &["verb_sein"], difficulty: 1, ..Ex::DEFAULT },
        Ex { seq: 2, section: "warmup", kind: "cloze", grading: "closed", prompt: "Schreib die Zahl als Wort: 7 = ___", accepted: &["sieben"], hints: &["It starts with s."], items: &["numbers_1_to_9"], difficulty: 1, ..Ex::DEFAULT },
        Ex { seq: 3, section: "review", kind: "translate", grading: "closed", prompt: "Übersetze ins Deutsche (mit Artikel): „сбор, пошлина“", accepted: &["die Gebühr"], model: "die Gebühr", hints: &["Feminin — die …", "It rhymes with Tür."], explanation: "die Gebühr, -en — was das Amt für seine Arbeit verlangt.", items: &["vocab_die_gebuehr", "article_gender_fem"], difficulty: 2, ..Ex::DEFAULT },
        Ex { seq: 4, section: "review", kind: "mcq", grading: "closed", prompt: "Was bedeutet „abgeben“ (einen Antrag abgeben)?", choices: &["сдать / to hand in", "получить / to receive", "отменить / to cancel", "verlängern"], accepted: &["сдать / to hand in"], explanation: "abgeben — trennbar: Ich gebe den Antrag ab.", items: &["vocab_abgeben"], difficulty: 2, ..Ex::DEFAULT },
        Ex { seq: 5, section: "review", kind: "listen_mcq", grading: "closed", prompt: "Wann ist der Termin?", audio: "Der Termin ist am Montag um neun Uhr.", choices: &["Montag um neun", "Dienstag um neun", "Montag um zehn", "Freitag um acht"], accepted: &["Montag um neun"], items: &["vocab_der_termin", "listening_times"], difficulty: 2, ..Ex::DEFAULT },
        Ex { seq: 6, section: "new", kind: "cloze", grading: "closed", prompt: "Ich bezahle die Gebühr nicht bar, sondern per ___. (= банковский перевод)", accepted: &["Überweisung"], model: "Ich bezahle die Gebühr per Überweisung.", hints: &["Von überweisen."], explanation: "die Überweisung — per Überweisung, ohne Artikel nach per.", items: &["vocab_die_ueberweisung"], difficulty: 3, ..Ex::DEFAULT },
        Ex { seq: 7, section: "set_piece", kind: "read_mcq", grading: "closed", prompt: "Was muss man zum Termin mitbringen?", passage: letter, choices: &["Reisepass, Passfoto, Einkommensnachweis", "Reisepass und Geburtsurkunde", "nur den Antrag", "Passfoto und Mietvertrag"], accepted: &["Reisepass, Passfoto, Einkommensnachweis"], items: &["vocab_der_nachweis", "vocab_das_passfoto"], difficulty: 3, ..Ex::DEFAULT },
        Ex { seq: 8, section: "set_piece", kind: "read_mcq", grading: "closed", prompt: "Wie viel kostet die Verlängerung, und wie bezahlt man?", passage: letter, choices: &["93 Euro, per Überweisung", "39 Euro, bar", "nichts", "130 Euro, per Überweisung"], accepted: &["93 Euro, per Überweisung"], items: &["vocab_die_gebuehr", "vocab_die_ueberweisung"], difficulty: 2, ..Ex::DEFAULT },
        Ex { seq: 9, section: "cooldown", kind: "free_write", grading: "self_check", prompt: "Schreib zwei Sätze: Was brauchst du, wenn du deinen Aufenthaltstitel verlängern möchtest? (Reisepass, Passfoto, Antrag, Gebühr)", model: "Ich brauche meinen Reisepass und ein Passfoto. Ich muss einen Antrag ausfüllen und die Gebühr bezahlen.", accepted: &["Ich brauche meinen Reisepass und ein aktuelles Passfoto. Ich muss einen Antrag stellen und eine Gebühr bezahlen."], items: &["writing_official", "vocab_der_antrag"], difficulty: 4, ..Ex::DEFAULT },
    ];
    for ex in ready {
        ex.insert(c, &uid(READY), None)?;
    }
    Ok(())
}

/// One exercise as the seed spells it.
struct Ex {
    seq: i64,
    section: &'static str,
    kind: &'static str,
    grading: &'static str,
    prompt: &'static str,
    passage: &'static str,
    audio: &'static str,
    choices: &'static [&'static str],
    accepted: &'static [&'static str],
    model: &'static str,
    hints: &'static [&'static str],
    explanation: &'static str,
    items: &'static [&'static str],
    difficulty: i64,
    answer: Option<&'static str>,
    result: Option<&'static str>,
    self_grade: Option<i64>,
    tutor_grade: Option<i64>,
    tutor_note: &'static str,
    tutor_fix: &'static str,
}

impl Ex {
    const DEFAULT: Ex = Ex {
        seq: 0,
        section: "review",
        kind: "mcq",
        grading: "closed",
        prompt: "",
        passage: "",
        audio: "",
        choices: &[],
        accepted: &[],
        model: "",
        hints: &[],
        explanation: "",
        items: &[],
        difficulty: 2,
        answer: None,
        result: None,
        self_grade: None,
        tutor_grade: None,
        tutor_note: "",
        tutor_fix: "",
    };

    /// An exercise names its lesson by the uid every device knows it by;
    /// the local id beside it is the schema's trigger's business.
    fn insert(&self, c: &Connection, lesson: &str, answered: Option<f64>) -> rusqlite::Result<()> {
        let json = |v: &[&str]| serde_json::to_string(v).unwrap_or_else(|_| "[]".into());
        c.execute(
            "INSERT INTO fluent_exercise(lesson_uid, seq, section, kind, grading, prompt, passage, audio, choices, accepted, model, hints, explanation, items, difficulty, answer, result, self_grade, tutor_grade, tutor_note, tutor_fix, elapsed, answered)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)",
            params![
                lesson,
                self.seq,
                self.section,
                self.kind,
                self.grading,
                self.prompt,
                self.passage,
                self.audio,
                json(self.choices),
                json(self.accepted),
                self.model,
                json(self.hints),
                self.explanation,
                json(self.items),
                self.difficulty,
                self.answer,
                self.result,
                self.self_grade,
                self.tutor_grade,
                self.tutor_note,
                self.tutor_fix,
                if answered.is_some() { 95.0 } else { 0.0 },
                answered
            ],
        )?;
        Ok(())
    }
}
