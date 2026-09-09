//! Calendar fields use the shared completion box, backed by cached data only.
use kernel::richtable::{Completion, Suggestion, MAX_SUGGESTIONS};
use kernel::store::Store;
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Field {
    #[default]
    Guests,
    Zone,
    Location,
    Repeat,
    Reminders,
    Duration,
    Time,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Context {
    field: Field,
    start: usize,
    end: usize,
    partial: String,
    taken: Vec<String>,
}
impl Completion for Field {
    type Ctx = Context;
    fn context(&self, text: &str, cursor: usize) -> Option<Context> {
        let mut cursor = cursor.min(text.len());
        while !text.is_char_boundary(cursor) {
            cursor -= 1;
        }
        let (mut start, end) = if *self == Self::Guests {
            (
                text[..cursor].rfind([',', ';', '\n']).map_or(0, |i| i + 1),
                text[cursor..]
                    .find([',', ';', '\n'])
                    .map_or(text.len(), |i| cursor + i),
            )
        } else {
            (0, text.len())
        };
        start += text[start..end].len() - text[start..end].trim_start().len();
        if *self == Self::Guests && text[start..end].starts_with('?') {
            start += 1;
        }
        let partial = text[start..cursor.max(start)].trim().to_lowercase();
        if partial.is_empty() && matches!(self, Self::Guests | Self::Location) {
            return None;
        }
        let taken = if *self == Self::Guests {
            format!("{},{}", &text[..start], &text[end..])
                .split([',', ';', '\n'])
                .map(|s| s.trim().trim_start_matches('?').to_lowercase())
                .collect()
        } else {
            Vec::new()
        };
        Some(Context {
            field: *self,
            start,
            end,
            partial,
            taken,
        })
    }
    fn offer(&self, store: &Store, ctx: &Context) -> Vec<Suggestion> {
        let mut choices = match self {
            Self::Guests => people(store),
            Self::Zone => chrono_tz::TZ_VARIANTS.iter().map(|tz| Suggestion::value(tz.name())).collect(),
            Self::Location => store.rows_sql("calendar locations", "recent event locations on connected calendars",
                "SELECT e.location FROM calendar_event e JOIN calendar_source c ON c.id=e.source JOIN account a ON a.id=c.account WHERE e.active=1 AND c.active=1 AND a.calendar_enabled=1 AND e.location<>'' GROUP BY e.location ORDER BY MAX(e.start) DESC LIMIT 200", &[], |r| r.get::<_, String>(0))
                .iter().map(Suggestion::value).collect(),
            Self::Repeat => [("Does not repeat", ""), ("Every day", "RRULE:FREQ=DAILY"), ("Every week", "RRULE:FREQ=WEEKLY"), ("Every weekday", "RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR"), ("Every month", "RRULE:FREQ=MONTHLY"), ("Every year", "RRULE:FREQ=YEARLY")]
                .into_iter().map(|(a,b)| Suggestion::labeled(a,b)).collect(),
            Self::Reminders => [("Calendar default", "default"), ("No reminders", ""), ("5 minutes before", "popup:5"), ("10 minutes before", "popup:10"), ("30 minutes before", "popup:30"), ("1 hour before", "popup:60"), ("1 day before", "popup:1440")]
                .into_iter().map(|(a,b)| Suggestion::labeled(a,b)).collect(),
            Self::Duration => [15,30,45,60,90,120,240,480].into_iter().map(|n| Suggestion::labeled(format!("{n} minutes"), n.to_string())).collect(),
            Self::Time => (0..48).map(|i| Suggestion::value(format!("{:02}:{:02}", i/2, (i%2)*30))).collect(),
        };
        let preset = matches!(
            self,
            Self::Repeat | Self::Reminders | Self::Duration | Self::Time
        ) && choices
            .iter()
            .any(|s| s.value.eq_ignore_ascii_case(&ctx.partial));
        if preset {
            if let Some(i) = choices
                .iter()
                .position(|s| s.value.eq_ignore_ascii_case(&ctx.partial))
            {
                choices.rotate_left(i);
            }
        }
        let typed = if preset {
            String::new()
        } else {
            ctx.partial.replace(' ', "_")
        };
        choices
            .into_iter()
            .filter(|s| {
                let value = s.value.to_lowercase();
                !ctx.taken.contains(&value)
                    && (*self != Self::Guests || value != ctx.partial)
                    && (value.contains(&typed)
                        || s.label.to_lowercase().contains(&ctx.partial)
                        || preset)
            })
            .map(|mut s| {
                if matches!(self, Self::Repeat | Self::Reminders | Self::Duration) {
                    s.describe.clear();
                }
                s
            })
            .take(MAX_SUGGESTIONS)
            .collect()
    }
    fn splice(&self, text: &str, _: usize, ctx: &Context, pick: &Suggestion) -> (String, usize) {
        let suffix = if *self == Self::Guests && ctx.end == text.len() {
            ", "
        } else {
            ""
        };
        (
            format!(
                "{}{}{}{}",
                &text[..ctx.start],
                pick.value,
                suffix,
                &text[ctx.end..]
            ),
            ctx.start + pick.value.len() + suffix.len(),
        )
    }
}

pub fn people(store: &Store) -> Vec<Suggestion> {
    let mut people = Vec::new();
    let rows = store.rows_sql("calendar people", "organizers and guests on connected calendars",
        "SELECT e.raw FROM calendar_event e JOIN calendar_source c ON c.id=e.source JOIN account a ON a.id=c.account WHERE e.active=1 AND c.active=1 AND a.calendar_enabled=1 ORDER BY e.start DESC LIMIT 2000", &[], |r| r.get::<_, String>(0));
    for row in rows.iter() {
        let Ok(raw) = serde_json::from_str::<serde_json::Value>(row) else {
            continue;
        };
        for person in raw["attendees"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(std::iter::once(&raw["organizer"]))
        {
            let email = super::model::text(person, "email");
            let name = super::model::text(person, "displayName");
            if !email.is_empty() {
                people.push(Suggestion::labeled(
                    if name.is_empty() { email } else { name },
                    email,
                ));
            }
        }
    }
    // Read Mail's public tables when that app is installed; Calendar-only
    // stores work without Mail. Never learn suggestions from the spam folder.
    if store
        .conn()
        .prepare("SELECT 1 FROM sqlite_master WHERE name='message'")
        .is_ok_and(|mut q| q.exists([]).unwrap_or(false))
    {
        let rows = store.rows_sql("calendar mail contacts", "non-spam correspondents, most recent first",
            "SELECT m.from_email,m.from_name FROM message m JOIN folder f ON f.id=m.folder JOIN account a ON a.id=f.account WHERE COALESCE(f.role,'')<>'spam' AND a.mail_enabled=1 GROUP BY lower(m.from_email) ORDER BY MAX(m.date) DESC LIMIT 2000", &[], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)));
        people.extend(rows.iter().map(|(email, name)| {
            Suggestion::labeled(if name.is_empty() { email } else { name }, email)
        }));
    }
    people.extend(
        store
            .rows_sql(
                "calendar account contacts",
                "connected account addresses",
                "SELECT email,label FROM account WHERE calendar_enabled=1 OR mail_enabled=1",
                &[],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .iter()
            .map(|(email, name)| {
                Suggestion::labeled(if name.is_empty() { email } else { name }, email)
            }),
    );
    let mut seen = HashSet::new();
    people.retain(|p| p.value.contains('@') && seen.insert(p.value.to_lowercase()));
    people
}

pub fn repeat_label(rule: &str) -> &str {
    match rule {
        "" => "does not repeat",
        "unchanged" => "keep current repeat",
        "RRULE:FREQ=DAILY" => "every day",
        "RRULE:FREQ=WEEKLY" => "every week",
        "RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR" => "every weekday",
        "RRULE:FREQ=MONTHLY" => "every month",
        "RRULE:FREQ=YEARLY" => "every year",
        _ => "custom repeat",
    }
}
