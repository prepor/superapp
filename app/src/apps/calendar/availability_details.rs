//! Event titles are optional enrichment of authoritative free/busy intervals.
//! They load after availability is published, without blocking slot selection.
use super::{
    api::{self, Request},
    availability::{BusyEvent, DetailState, Details, Person, Query, ResultSet},
    dates, model,
};
use kernel::effect::World;
use rusqlite::{params, OptionalExtension};
use serde_json::Value;

pub fn event(value: &Value, zone: &str, account: &str) -> Option<BusyEvent> {
    if value["status"] == "cancelled"
        || value["transparency"] == "transparent"
        || value["attendees"].as_array().is_some_and(|guests| {
            guests
                .iter()
                .any(|g| g["self"] == true && g["responseStatus"] == "declined")
        })
    {
        return None;
    }
    let (start, all_day) = dates::read(&value["start"], zone).ok()?;
    let (end, _) = dates::read(&value["end"], zone).ok()?;
    (end > start).then(|| BusyEvent {
        id: model::text(value, "id").into(),
        // Google omits restricted fields. Never fill them from another event
        // or guess a title from an occupied interval.
        title: model::text(value, "summary").into(),
        location: model::text(value, "location").into(),
        start,
        end,
        all_day,
        account: account.into(),
    })
}

async fn lookup(
    w: &World,
    q: &Query,
    person: &Person,
    account: &str,
) -> Result<Vec<BusyEvent>, String> {
    let (a, b) = q.validate()?;
    let request = Request::get(account, &api::events(&person.calendar))
        .query("singleEvents", true)
        .query("timeMin", dates::rfc(a))
        .query("timeMax", dates::rfc(b))
        .query("maxResults", 2500)
        .query("fields", "nextPageToken,timeZone,items(id,summary,location,start,end,status,transparency,attendees(self,responseStatus))");
    let mut events = Vec::new();
    let mut next = String::new();
    let mut seen = std::collections::HashSet::new();
    loop {
        let mut request = request.clone();
        if !next.is_empty() {
            request = request.query("pageToken", &next);
        }
        let page = w.run_async(&request).await?;
        if !page.is_object() {
            return Err("Google returned invalid event details".into());
        }
        let zone = page["timeZone"].as_str().unwrap_or(&q.zone);
        if let Some(items) = page.get("items") {
            for value in items
                .as_array()
                .ok_or("Google returned invalid event details")?
            {
                if let Some(event) = event(value, zone, account).filter(|event| {
                    event.start < b
                        && event.end > a
                        && person
                            .busy
                            .iter()
                            .any(|(start, end)| event.start < *end && event.end > *start)
                }) {
                    events.push(event);
                }
            }
        }
        if events.len() > 10_000 {
            return Err("too many event details in this date range".into());
        }
        next = model::text(&page, "nextPageToken").into();
        if next.is_empty() {
            return Ok(events);
        }
        if !seen.insert(next.clone()) {
            return Err("Google repeated an event-details page".into());
        }
    }
}

fn explained(person: &Person, events: &[BusyEvent]) -> bool {
    person.busy.iter().all(|(start, end)| {
        let mut through = *start;
        for event in events.iter().filter(|e| !e.title.is_empty()) {
            if event.start <= through {
                through = through.max(event.end);
            }
        }
        through >= *end
    })
}

async fn fetch(w: &World, q: &Query, person: &Person) -> Details {
    let mut accounts = w
        .store()
        .rows_sql(
            "availability detail accounts",
            "connected identities for shared event details",
            "SELECT email FROM account WHERE calendar_enabled=1 ORDER BY id",
            &[],
            |r| r.get::<_, String>(0),
        )
        .as_ref()
        .clone();
    accounts.sort_by_key(|email| {
        person
            .via()
            .is_none_or(|via| !via.eq_ignore_ascii_case(email))
    });
    let mut details = Details::default();
    for account in accounts {
        match lookup(w, q, person, &account).await {
            Ok(events) => {
                details.state = DetailState::Ready;
                for event in events {
                    if let Some(existing) = details.events.iter_mut().find(|old| {
                        (!event.id.is_empty() && event.id == old.id)
                            || (event.start == old.start
                                && event.end == old.end
                                && event.title == old.title)
                    }) {
                        if existing.title.is_empty() && !event.title.is_empty() {
                            *existing = event;
                        }
                    } else {
                        details.events.push(event);
                    }
                }
                details.events.sort_by(|a, b| a.start.total_cmp(&b.start));
                if explained(person, &details.events) {
                    break;
                }
            }
            Err(error) => details.error = error,
        }
    }
    if details.state == DetailState::Ready {
        details.error.clear();
    }
    details
}

/// One participant per pass. Old serialized results default to unavailable,
/// so opening history does not start new lookups for historical guests.
pub async fn pass(w: &World) -> Result<bool, String> {
    let row = w.store().conn().query_row(
        "SELECT id,request,response FROM calendar_availability WHERE response IS NOT NULL AND checked>=?1 AND EXISTS(SELECT 1 FROM json_each(response,'$.people') WHERE json_extract(value,'$.details.state')='pending') ORDER BY id DESC LIMIT 1",
        [w.now() - 300.0],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
    ).optional().map_err(|e| e.to_string())?;
    let Some((id, query, body)) = row else {
        return Ok(false);
    };
    let q: Query = serde_json::from_str(&query).map_err(|e| e.to_string())?;
    let mut result: ResultSet = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let Some(person) = result
        .people
        .iter_mut()
        .find(|p| p.details.state == DetailState::Pending)
    else {
        return Ok(false);
    };
    person.details = fetch(w, &q, person).await;
    w.store()
        .write_async(move |c| {
            c.execute(
                "UPDATE calendar_availability SET response=?1 WHERE id=?2 AND response=?3",
                params![serde_json::to_string(&result).unwrap(), id, body],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?;
    Ok(true)
}
