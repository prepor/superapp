/* A UI review fixture. All changes stay in memory. No Google API calls, tokens,
   real event links, invitations, or application database access. */
"use strict";

const TODAY = "2026-09-08";
const NOW = Date.parse("2026-09-08T09:41:00+02:00");
const DISPLAY_ZONE = "Europe/Berlin";
const workspace = document.getElementById("workspace");
const escapeHtml = (value) => String(value == null ? "" : value).replace(/[&<>"']/g, (char) => ({
  "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;"
})[char]);
const attr = escapeHtml;
const clone = (value) => JSON.parse(JSON.stringify(value));
const dateAtNoon = (day) => new Date(day + "T12:00:00Z");
const validDay = (day) => /^\d{4}-\d{2}-\d{2}$/.test(day) &&
  Number.isFinite(dateAtNoon(day).getTime()) && dateAtNoon(day).toISOString().slice(0, 10) === day;
const addDays = (day, count) => new Date(dateAtNoon(day).getTime() + count * 86400000).toISOString().slice(0, 10);
const dayDifference = (a, b) => Math.round((dateAtNoon(a) - dateAtNoon(b)) / 86400000);
const prettyDate = (day, options = { weekday: "long", day: "numeric", month: "long" }) =>
  Number.isFinite(dateAtNoon(day).getTime())
    ? new Intl.DateTimeFormat("en-GB", { ...options, timeZone: "UTC" }).format(dateAtNoon(day))
    : "…";
const clockTime = (minutes) => String(Math.floor(minutes / 60)).padStart(2, "0") + ":" + String(minutes % 60).padStart(2, "0");
const toMinutes = (time) => Number(time.slice(0, 2)) * 60 + Number(time.slice(3, 5));

// Keep event-local wall times distinct from the timeline's display zone.
function zonedInstant(day, time, zone) {
  const wall = Date.parse(day + "T" + time + ":00Z");
  const formatter = new Intl.DateTimeFormat("en-GB", {
    timeZone: zone, year: "numeric", month: "2-digit", day: "2-digit",
    hour: "2-digit", minute: "2-digit", second: "2-digit", hourCycle: "h23"
  });
  let instant = wall;
  for (let i = 0; i < 3; i++) {
    const parts = Object.fromEntries(formatter.formatToParts(new Date(instant)).map((part) => [part.type, part.value]));
    const represented = Date.parse(parts.year + "-" + parts.month + "-" + parts.day + "T" + parts.hour + ":" + parts.minute + ":" + parts.second + "Z");
    instant += wall - represented;
  }
  return instant;
}

function inDisplayZone(instant) {
  const parts = Object.fromEntries(new Intl.DateTimeFormat("en-GB", {
    timeZone: DISPLAY_ZONE, year: "numeric", month: "2-digit", day: "2-digit",
    hour: "2-digit", minute: "2-digit", hourCycle: "h23"
  }).formatToParts(new Date(instant)).map((part) => [part.type, part.value]));
  return { date: parts.year + "-" + parts.month + "-" + parts.day, time: parts.hour + ":" + parts.minute };
}

function presented(event) {
  if (event.allDay) return { date: event.date, endDate: event.endDate, start: "", end: "" };
  const start = inDisplayZone(zonedInstant(event.date, event.start, event.zone));
  const end = inDisplayZone(zonedInstant(event.endDate, event.end, event.zone));
  return { date: start.date, endDate: end.date, start: start.time, end: end.time };
}

function occursOn(event, day) {
  if (!validDay(day)) return false;
  if (event.allDay) return event.date <= day && event.endDate >= day;
  return zonedInstant(event.date, event.start, event.zone) < zonedInstant(addDays(day, 1), "00:00", DISPLAY_ZONE) &&
    zonedInstant(event.endDate, event.end, event.zone) > zonedInstant(day, "00:00", DISPLAY_ZONE);
}

function zoneOffset(day, zone) {
  const value = new Intl.DateTimeFormat("en-GB", { timeZone: zone, timeZoneName: "shortOffset" })
    .formatToParts(dateAtNoon(validDay(day) ? day : TODAY)).find((part) => part.type === "timeZoneName").value;
  return value.replace(/GMT([+-])(\d{1,2})(?::(\d{2}))?/, (_, sign, hours, minutes) =>
    "UTC" + sign + hours.padStart(2, "0") + (minutes ? ":" + minutes : "")).replace("GMT", "UTC");
}

const initialAccounts = [
  { id: "work", label: "Work", email: "andrey@studio.example", provider: "Google", mail: true, calendar: true },
  { id: "personal", label: "Personal", email: "andrey@example.com", provider: "Google", mail: true, calendar: true },
  { id: "other", label: "Other mail", email: "hello@rudenko.example", provider: "IMAP / SMTP", mail: true, calendar: false }
];
const initialCalendars = [
  { id: "work", account: "work", name: "Work", mark: "■", role: "owner", selected: true },
  { id: "team", account: "work", name: "Studio", mark: "▧", role: "reader", selected: true },
  { id: "personal", account: "personal", name: "Personal", mark: "◇", role: "owner", selected: true }
];
const people = {
  "andrey@studio.example": { name: "You", initials: "AR", status: "organizer" },
  "andrey@example.com": { name: "You", initials: "AR", status: "organizer" },
  "nora@studio.example": { name: "Nora Chen", initials: "NC", status: "accepted" },
  "leo@studio.example": { name: "Leo Martin", initials: "LM", status: "accepted" },
  "ava@partner.example": { name: "Ava Wilson", initials: "AW", status: "awaiting reply" },
  "mila@example.com": { name: "Mila", initials: "MK", status: "accepted" }
};

function sampleEvent(id, title, date, start, end, calendar, extras = {}) {
  return {
    id, title, date, endDate: date, start, end, calendar, zone: DISPLAY_ZONE,
    allDay: false, meet: false, guests: [], location: "", description: "",
    recurrence: "none", series: null, response: "accepted", organizer: "andrey@studio.example",
    busy: "busy", visibility: "default", reminder: "10", notify: true, ...extras
  };
}

const initialEvents = [
  sampleEvent("birthday", "Mila’s birthday", TODAY, "", "", "personal", { allDay: true, busy: "free", organizer: "andrey@example.com" }),
  sampleEvent("morning", "Morning pages", TODAY, "08:00", "08:30", "personal"),
  sampleEvent("review-1", "Design review", TODAY, "10:00", "10:45", "work", {
    meet: true, guests: ["nora@studio.example", "leo@studio.example", "ava@partner.example"],
    recurrence: "weekly", series: "design-review",
    description: "A first look at the calendar.\n\nWalk through the timeline and event editor. Bring any awkward scheduling cases — especially meetings across accounts.",
    location: "Studio · Meeting room 2"
  }),
  sampleEvent("research", "Research catch-up", TODAY, "11:30", "12:00", "work", {
    meet: true, guests: ["andrey@studio.example", "nora@studio.example"], response: "needs-action",
    organizer: "nora@studio.example", description: "Notes from the last five customer interviews. Choose the next questions together."
  }),
  sampleEvent("delivery", "Delivery window", TODAY, "11:45", "12:15", "personal", { location: "Home", organizer: "andrey@example.com", visibility: "private" }),
  sampleEvent("lunch", "Lunch with Mila", TODAY, "13:00", "14:00", "personal", { location: "Café Frieda", guests: ["mila@example.com"], organizer: "andrey@example.com" }),
  sampleEvent("planning", "Product planning", "2026-09-09", "09:30", "10:00", "work", { meet: true, guests: ["nora@studio.example", "leo@studio.example"] }),
  sampleEvent("focus", "Time to make things", "2026-09-09", "10:00", "12:00", "work", { visibility: "private", description: "An uninterrupted stretch for the calendar prototype." }),
  sampleEvent("dinner", "Dinner with friends", "2026-09-09", "19:00", "21:00", "personal", { organizer: "andrey@example.com", location: "Mila’s place" }),
  sampleEvent("allhands", "Studio all-hands", "2026-09-10", "15:00", "16:00", "team", { meet: true, organizer: "nora@studio.example", description: "What we shipped, what we learned, and what is next." }),
  sampleEvent("walk", "Walk & talk", "2026-09-11", "10:00", "10:45", "work", { guests: ["leo@studio.example"], location: "By the canal" }),
  sampleEvent("weekend", "A weekend away", "2026-09-12", "", "", "personal", { allDay: true, endDate: "2026-09-13", organizer: "andrey@example.com" }),
  sampleEvent("review-2", "Design review", "2026-09-15", "10:00", "10:45", "work", { meet: true, guests: ["nora@studio.example", "leo@studio.example", "ava@partner.example"], recurrence: "weekly", series: "design-review" }),
  sampleEvent("dentist", "Dentist", "2026-09-17", "09:00", "09:45", "personal", { organizer: "andrey@example.com", visibility: "private", location: "Praxis am Park" }),
  sampleEvent("review-3", "Design review", "2026-09-22", "10:00", "10:45", "work", { meet: true, guests: ["nora@studio.example", "leo@studio.example", "ava@partner.example"], recurrence: "weekly", series: "design-review" }),
  sampleEvent("demo", "September demo", "2026-09-25", "14:00", "15:00", "team", { meet: true, organizer: "nora@studio.example" })
];

let accounts;
let calendars;
let events;
let state;
let undoSnapshot = null;
let toastTimer;

function reset() {
  accounts = clone(initialAccounts);
  calendars = clone(initialCalendars);
  events = clone(initialEvents);
  state = {
    scene: "timeline", view: "timeline", query: "", range: 30,
    month: "2026-09-01", selectedDate: TODAY, selectedEvent: "review-1",
    side: "event", account: "work", editor: null, availability: false,
    availabilityDate: "2026-09-09", duration: 30, hours: "work", selectedSlot: null,
    focus: "main", deleteScope: "this"
  };
  undoSnapshot = null;
  render();
}

function accountFor(calendar) { return accounts.find((account) => account.id === calendar.account); }
function calendarFor(event) { return calendars.find((calendar) => calendar.id === event.calendar); }
function availableCalendars() { return calendars.filter((calendar) => accountFor(calendar)?.calendar); }
function visibleCalendars() { return availableCalendars().filter((calendar) => calendar.selected); }
function isWritable(event) {
  const calendar = calendarFor(event);
  return calendar && calendar.role !== "reader" && event.organizer === accountFor(calendar)?.email;
}
function remember() { undoSnapshot = { events: clone(events), accounts: clone(accounts), calendars: clone(calendars) }; }

function notify(message, undo = false) {
  const toast = document.getElementById("toast");
  clearTimeout(toastTimer);
  toast.innerHTML = escapeHtml(message) + (undo ? '<button data-action="undo">Undo</button>' : "");
  toast.classList.add("visible");
  toastTimer = setTimeout(() => toast.classList.remove("visible"), 6500);
}

function action(label, name, data = "", primary = false) {
  return '<button class="action' + (primary ? " primary" : "") + '" data-action="' + attr(name) + '"' + data + ">" + label + "</button>";
}
function link(label, name, replace = false, data = "") {
  return '<button class="nav-link' + (replace ? " replace" : "") + '" data-action="' + attr(name) + '"' + data + ">" + label + "</button>";
}
function panel(id, title, className, body, bar = "", beforeBody = "") {
  return '<section id="' + id + '" class="panel ' + className + (state.focus === id ? " focused" : "") + '" aria-label="' + attr(title) + '">' +
    '<header class="panel-header"><span>' + escapeHtml(title) + '</span><button class="close-panel" aria-label="Close ' + attr(title) + '" data-action="close" data-panel="' + id + '">×</button></header>' +
    beforeBody + '<div class="panel-body">' + body + "</div>" +
    (bar ? '<footer class="panel-bar">' + bar + "</footer>" : "") + "</section>";
}

function matchingEvents(month = false) {
  const visible = new Set(visibleCalendars().map((calendar) => calendar.id));
  const terms = state.query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  return events.filter((event) => {
    if (!visible.has(event.calendar)) return false;
    const when = presented(event);
    if (month) {
      if (when.date >= addDays(state.month, 42) || when.endDate < addDays(state.month, -7)) return false;
    } else {
      if (event.allDay ? event.endDate < TODAY : zonedInstant(event.endDate, event.end, event.zone) <= NOW) return false;
      if (state.range && when.date >= addDays(TODAY, state.range)) return false;
    }
    const calendar = calendarFor(event);
    const account = accountFor(calendar);
    const participants = [event.organizer, ...event.guests];
    const haystack = [event.title, event.description, event.location, ...participants, ...participants.map((email) => people[email]?.name || "")].join(" ").toLowerCase();
    return terms.every((term) => {
      if (term === "@meet") return event.meet;
      if (term === "@rsvp:needs-action") return event.response === "needs-action";
      if (term.startsWith("@calendar:")) return [calendar.id, calendar.name.toLowerCase()].includes(term.slice(10));
      if (term.startsWith("@account:")) return [account.id, account.email.toLowerCase(), account.label.toLowerCase()].includes(term.slice(9));
      if (term.startsWith("@with:")) return [event.organizer, ...event.guests].some((email) => (email + " " + (people[email]?.name || "")).toLowerCase().includes(term.slice(6)));
      if (term.startsWith("@date:")) return /^\d{4}-\d{2}-\d{2}$/.test(term.slice(6)) && occursOn(event, term.slice(6));
      if (term.startsWith("@")) return false;
      return haystack.includes(term);
    });
  }).sort((a, b) => {
    const left = presented(a), right = presented(b);
    return left.date.localeCompare(right.date) || Number(b.allDay) - Number(a.allDay) || left.start.localeCompare(right.start) || a.id.localeCompare(b.id);
  });
}

function overlaps(event) {
  if (event.allDay || event.busy === "free" || event.response === "declined") return false;
  const start = zonedInstant(event.date, event.start, event.zone);
  const end = zonedInstant(event.endDate, event.end, event.zone);
  return events.some((other) => {
    if (other.id === event.id || other.allDay || other.busy === "free" || other.response === "declined") return false;
    const calendar = calendarFor(other);
    if (!calendar || calendar.role === "reader" || !accountFor(calendar)?.calendar) return false;
    return start < zonedInstant(other.endDate, other.end, other.zone) && end > zonedInstant(other.date, other.start, other.zone);
  });
}

function eventRow(event) {
  const calendar = calendarFor(event);
  const when = presented(event);
  const duration = event.allDay ? "all day" : when.end + (when.endDate !== when.date ? " +" + dayDifference(when.endDate, when.date) + "d" : "");
  return '<button class="event-row' + (event.id === state.selectedEvent && state.side === "event" ? " selected" : "") + '" data-action="event" data-id="' + attr(event.id) + '" aria-label="' + attr(event.title + ", " + (event.allDay ? "all day" : when.start) + ", " + calendar.name) + '">' +
    '<span class="event-time">' + (event.allDay ? "all day" : when.start) + '<span class="event-end">' + (event.allDay ? (event.date !== event.endDate ? "→ " + prettyDate(event.endDate, { day: "numeric", month: "short" }) : "") : duration) + "</span></span>" +
    '<span><span class="event-title">' + escapeHtml(event.title) + '</span><span class="event-sub"><span><span class="event-mark">' + calendar.mark + "</span>" + escapeHtml(calendar.name) + "</span>" +
    (event.meet ? "<span>↗ Meet</span>" : event.location ? "<span>" + escapeHtml(event.location) + "</span>" : "") +
    (event.recurrence !== "none" ? '<span title="Recurring event">↻ weekly</span>' : "") +
    (event.response === "needs-action" ? '<span class="event-tag">needs reply</span>' : "") +
    (event.response === "declined" ? '<span class="event-tag">declined</span>' : "") +
    (overlaps(event) ? '<span class="event-tag conflict">overlap</span>' : "") +
    (calendar.role === "reader" ? '<span class="event-tag">read only</span>' : "") + "</span></span></button>";
}

function emptyEvents() {
  const noSources = !visibleCalendars().length;
  return '<div class="empty-state"><h2>' + (noSources ? "No calendars selected" : "Nothing here for now") + "</h2><p>" +
    (noSources ? "Choose which calendars you want to see." : state.query ? "No events match these filters. Try another word or clear the filter." : "There are no upcoming events in this range.") +
    "</p>" + (noSources ? link("choose calendars", "sources") : state.query ? action("clear filter", "clear-filter") : link("new event", "new")) + "</div>";
}

function agendaHtml(list, dayOnly = null) {
  if (!list.length) return emptyEvents();
  const days = new Map();
  list.forEach((event) => {
    const when = presented(event);
    const day = dayOnly || (when.date < TODAY && !dayOnly ? TODAY : when.date);
    if (!days.has(day)) days.set(day, []);
    days.get(day).push(event);
  });
  return Array.from(days, ([day, group]) => '<section class="day-group" aria-label="' + attr(prettyDate(day)) + '">' +
    '<div class="day-label"><div class="day-name">' + prettyDate(day, { weekday: "short" }) + '</div><div class="day-num">' + day.slice(8) + "</div>" +
    (day === TODAY ? '<div class="today-mark">Today</div>' : '<span class="small muted">' + prettyDate(day, { month: "short" }) + "</span>") + "</div>" +
    '<div class="day-events">' + (day === TODAY && !dayOnly ? '<div class="timeline-now">09:41 · now</div>' : "") +
    group.map(eventRow).join("") + '<div class="day-summary">' + group.length + " event" + (group.length === 1 ? "" : "s") + "</div></div></section>").join("");
}

function filterTools(month = false) {
  const tagOn = (tag) => state.query.split(/\s+/).includes(tag);
  return '<div class="list-tools' + (month ? " month-tools" : "") + '"><div class="list-heading"><div><h1>' + (month ? prettyDate(state.month, { month: "long", year: "numeric" }) : "Up next") + "</h1>" +
    (month ? '<div class="month-count">Your calendars, one month at a time.</div>' : "") + "</div>" +
    (month ? '<div class="month-navigation">' + action("‹", "prev-month", ' aria-label="Previous month"') + action("›", "next-month", ' aria-label="Next month"') + "</div>" :
      '<select id="range" aria-label="Event date range"><option value="7"' + (state.range === 7 ? " selected" : "") + '>Next 7 days</option><option value="30"' + (state.range === 30 ? " selected" : "") + '>Next 30 days</option><option value="0"' + (!state.range ? " selected" : "") + ">All upcoming</option></select>") +
    '</div><div class="filter-wrap"><input id="event-filter" type="search" aria-label="Filter events" placeholder="Filter events…  @calendar:work  @with:nora" value="' + attr(state.query) + '"><span class="filter-key">/</span></div>' +
    '<div class="quick-filters"><button class="quick-filter" data-action="filter-tag" data-tag="@rsvp:needs-action" aria-pressed="' + tagOn("@rsvp:needs-action") + '"><span class="tiny-square"></span>Needs reply</button>' +
    '<button class="quick-filter" data-action="filter-tag" data-tag="@meet" aria-pressed="' + tagOn("@meet") + '"><span class="tiny-square"></span>With Meet</button>' +
    '<span class="bar-spacer"></span><button class="nav-link" data-action="sources">calendars ' + visibleCalendars().length + "/" + availableCalendars().length + "</button></div></div>";
}

function timelinePanel() {
  const list = matchingEvents();
  const top = filterTools() + '<div class="range-caption"><span id="result-count">' + list.length + ' upcoming events</span><span>Europe/Berlin · UTC+02</span></div>';
  return panel("main", "calendar · timeline", "primary-panel", '<div id="agenda" class="agenda">' + agendaHtml(list) + "</div>",
    action("sync", "sync") + link("new event", "new") + link("month", "month", true) + link("accounts", "accounts") + '<span class="bar-spacer"></span><span class="small muted">sample · just synced</span>', top);
}

function monthPanel() {
  const firstDay = (dateAtNoon(state.month).getUTCDay() + 6) % 7;
  const nextMonth = new Date(dateAtNoon(state.month));
  nextMonth.setUTCMonth(nextMonth.getUTCMonth() + 1);
  const count = Math.ceil((firstDay + dayDifference(nextMonth.toISOString().slice(0, 10), state.month)) / 7) * 7;
  const list = matchingEvents(true);
  const cells = Array.from({ length: count }, (_, index) => {
    const day = addDays(state.month, index - firstDay);
    const dayEvents = list.filter((event) => occursOn(event, day));
    return '<div class="month-cell' + (day.slice(0, 7) !== state.month.slice(0, 7) ? " outside" : "") + (index % 7 >= 5 ? " weekend" : "") + (day === state.selectedDate ? " chosen" : "") + '">' +
      '<button class="day-number' + (day === TODAY ? " today" : "") + '" data-action="day" data-date="' + day + '" aria-label="' + attr(prettyDate(day)) + '">' + Number(day.slice(8)) + "</button>" +
      dayEvents.slice(0, 3).map((event) => '<button class="month-event ' + event.calendar + (event.id === state.selectedEvent ? " selected" : "") + '" data-action="event" data-id="' + attr(event.id) + '" title="' + attr(event.title) + '">' + (event.allDay ? "" : presented(event).start + " ") + escapeHtml(event.title) + "</button>").join("") +
      (dayEvents.length > 3 ? '<button class="more-events" data-action="day" data-date="' + day + '">+' + (dayEvents.length - 3) + " more</button>" : "") + "</div>";
  }).join("");
  const top = filterTools(true) + '<div class="week-head">' + ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"].map((day) => "<div>" + day + "</div>").join("") + "</div>";
  // The grid stretches to the body's available height, keeping all weeks visible.
  return panel("main", "calendar · month", "month-panel", '<div class="month-grid">' + cells + "</div>",
    action("today", "today") + link("timeline", "timeline", true) + link("new event", "new") + link("accounts", "accounts"), top).replace('class="panel-body"', 'class="panel-body month-body"');
}

function personRow(email, organizer) {
  const person = people[email] || { name: email.split("@")[0], initials: email.slice(0, 2).toUpperCase(), status: "awaiting reply" };
  const status = email === organizer ? "organizer" : person.status === "organizer" ? "accepted" : person.status;
  return '<div class="person-row"><span class="person-avatar" aria-hidden="true">' + escapeHtml(person.initials) + '</span><div><div class="person-name">' + escapeHtml(person.name) + '</div><div class="person-email">' + escapeHtml(email) + '</div></div><span class="person-status">' + (status === "accepted" ? "✓ " : "") + escapeHtml(status) + "</span></div>";
}

function eventPanel() {
  const event = events.find((item) => item.id === state.selectedEvent);
  if (!event) return panel("side", "event", "side-panel joined", '<div class="empty-state"><h2>Event removed</h2><p>Choose another event from the timeline.</p></div>');
  const calendar = calendarFor(event);
  const account = accountFor(calendar);
  const when = presented(event);
  const own = isWritable(event);
  const attendees = Array.from(new Set([event.organizer, ...event.guests]));
  const duration = event.allDay ? "" : Math.round((zonedInstant(event.endDate, event.end, event.zone) - zonedInstant(event.date, event.start, event.zone)) / 60000);
  const response = event.response === "needs-action" ? "Awaiting your reply" : event.response === "tentative" ? "You replied maybe" : event.response === "declined" ? "You declined" : "You’re going";
  let body = '<div class="detail-content"><div class="eyebrow"><span>' + calendar.mark + " " + escapeHtml(calendar.name) + "</span>" +
    (event.recurrence !== "none" ? '<span class="tag">↻ recurring</span>' : "") +
    (calendar.role === "reader" ? '<span class="tag">read only</span>' : "") + "</div><h2>" + escapeHtml(event.title) + '</h2><div class="date-line">' + prettyDate(when.date) +
    (when.endDate !== when.date ? " – " + prettyDate(when.endDate) : "") + '</div><div class="big-time">' + (event.allDay ? "All day" : when.start + " — " + when.end) +
    '</div><div class="time-meta">' + (event.allDay ? "Date only · no time zone conversion" : duration + " min · Europe/Berlin (" + zoneOffset(when.date, DISPLAY_ZONE) + ")") + "</div>" +
    (event.recurrence !== "none" ? '<div class="time-meta">Repeats every ' + prettyDate(event.date, { weekday: "long" }) + "</div>" : "") +
    (event.meet ? '<div class="meet-card"><div><div class="meet-title"><span class="camera-icon" aria-hidden="true"></span>Google Meet</div><div class="meet-address">Sample meeting link</div></div>' + action("join ↗", "join-meet") + "</div>" : "") +
    '<div class="detail-section"><dl class="metadata"><dt>Calendar</dt><dd>' + calendar.mark + " " + escapeHtml(calendar.name) + " · " + escapeHtml(account.email) + "</dd>" +
    (event.location ? "<dt>Location</dt><dd>" + escapeHtml(event.location) + "</dd>" : "") +
    "<dt>Show as</dt><dd>" + (event.busy === "busy" ? "Busy" : "Free") + "</dd><dt>Visibility</dt><dd>" + (event.visibility === "default" ? "Calendar default" : event.visibility === "private" ? "Private" : "Public") +
    "</dd><dt>Reminder</dt><dd>" + (event.reminder === "none" ? "None" : event.reminder + " minutes before") + "</dd></dl></div>" +
    '<div class="detail-section"><div class="section-heading"><h3>' + attendees.length + " participant" + (attendees.length === 1 ? "" : "s") + "</h3>" + (own && attendees.length > 1 ? link("find a time", "find-time") : "") + "</div>" +
    attendees.map((email) => personRow(email, event.organizer)).join("") + "</div>" +
    (event.description ? '<div class="detail-section"><h3>Notes</h3><div class="prose">' + event.description.split("\n\n").map((paragraph) => "<p>" + escapeHtml(paragraph).replace(/\n/g, "<br>") + "</p>").join("") + "</div></div>" : "");
  if (overlaps(event)) body += '<div class="detail-section"><div class="notice">This overlaps another event on one of your connected calendars.</div></div>';
  if (!own && calendar.role !== "reader") body += '<div class="detail-section"><h3>Your response</h3><p class="small secondary">' + response + "</p></div>";
  if (calendar.role === "reader") body += '<div class="detail-bottom">You can view this calendar. Only its editors can change events.</div>';
  body += '<div class="detail-bottom">Google Calendar · sample event</div></div>';
  const bar = own ? link("edit", "edit", true) + action("delete", "delete") + link("duplicate", "duplicate") :
    calendar.role === "reader" ? link("duplicate", "duplicate") : action("yes", "rsvp", ' data-response="accepted"') + action("maybe", "rsvp", ' data-response="tentative"') + action("no", "rsvp", ' data-response="declined"') + link("duplicate", "duplicate");
  return panel("side", "event · " + calendar.name, "side-panel joined", body, bar + '<span class="bar-spacer"></span>' + link("ask agent", "agent"));
}

function dayPanel() {
  const day = state.selectedDate;
  const list = matchingEvents(true).filter((event) => occursOn(event, day));
  const body = '<div class="detail-content"><div class="eyebrow">' + prettyDate(day, { weekday: "long" }) + "</div><h2>" + prettyDate(day, { day: "numeric", month: "long" }) +
    '</h2><p class="small secondary">' + list.length + ' events · Europe/Berlin</p><div class="mini-agenda">' +
    (list.length ? agendaHtml(list, day) : '<div class="empty-state"><h2>A little room in the day.</h2><p>No events match your selected calendars and filters.</p></div>') + "</div></div>";
  return panel("side", "day · " + prettyDate(day, { day: "numeric", month: "short" }), "side-panel joined", body, link("new event on this day", "new-on-day"));
}

function options(values, selected) {
  return values.map(([value, label]) => '<option value="' + attr(value) + '"' + (String(selected) === String(value) ? " selected" : "") + ">" + escapeHtml(label) + "</option>").join("");
}
function field(label, control, note = "") {
  return '<label class="field"><span>' + label + "</span>" + control + (note ? '<span class="field-note">' + note + "</span>" : "") + "</label>";
}
function input(name, value, type = "text", extra = "") {
  return '<input name="' + name + '" type="' + type + '" value="' + attr(value) + '"' + extra + ">";
}
function select(name, values, value) { return '<select name="' + name + '">' + options(values, value) + "</select>"; }

function newEditor(day = TODAY) {
  const calendar = availableCalendars().find((item) => item.role !== "reader");
  if (!calendar) { state.side = "sources"; notify("Connect a writable calendar to create an event."); return; }
  state.editor = {
    ...sampleEvent("", "", day, "14:00", "14:30", calendar.id),
    organizer: accountFor(calendar).email, scope: "this"
  };
  state.side = "editor";
  state.availability = false;
  state.focus = "side";
}

function editSelected() {
  const event = events.find((item) => item.id === state.selectedEvent);
  if (!event || !isWritable(event)) return false;
  state.editor = { ...clone(event), scope: "this" };
  state.side = "editor";
  state.focus = "side";
  return true;
}

function recurrenceScope(name, selected) {
  return '<div class="scope-options">' + [["this", "This event"], ["following", "This and following events"], ["all", "All events in the series"]].map(([value, label]) =>
    '<label><input type="radio" name="' + name + '" value="' + value + '"' + (selected === value ? " checked" : "") + "> " + label + "</label>").join("") + "</div>";
}

function editorPanel() {
  const draft = state.editor;
  const calendarOptions = availableCalendars().filter((calendar) => calendar.role !== "reader").map((calendar) => [calendar.id, calendar.name + " · " + accountFor(calendar).email]);
  const body = '<form id="event-form" class="form-content">' +
    field("Event", input("title", draft.title, "text", ' class="title-input" placeholder="What’s the occasion?" required maxlength="180"')) +
    field("Calendar", select("calendar", calendarOptions, draft.calendar)) +
    '<div class="form-grid">' + field("Starts", input("date", draft.date, "date", " required")) + field("Ends", input("endDate", draft.endDate, "date", " required")) + "</div>" +
    '<label class="checkbox-field"><input type="checkbox" name="allDay"' + (draft.allDay ? " checked" : "") + ">All day</label>" +
    '<div class="form-grid">' + field("From", input("start", draft.start || "09:00", "time", draft.allDay ? " disabled" : " required")) +
    field("To", input("end", draft.end || "09:30", "time", draft.allDay ? " disabled" : " required")) + "</div>" +
    field("Time zone", select("zone", ["Europe/Berlin", "Europe/London", "America/New_York"].map((zone) => [zone, zone + " · " + zoneOffset(draft.date, zone)]), draft.zone)) +
    field("Repeat", select("recurrence", [["none", "Does not repeat"], ["weekly", "Every week on " + prettyDate(draft.date, { weekday: "long" })]], draft.recurrence),
      draft.recurrence !== "none" && !draft.series ? "Preview: three weekly occurrences will be added." : "") +
    (draft.series ? '<div class="notice"><span class="field-title">Apply changes to</span>' + recurrenceScope("scope", draft.scope) + "</div>" : "") +
    '<hr class="form-rule"><div class="field"><div class="section-heading"><span class="field-title">Guests</span>' + link("find a time", "find-time") + '</div><div class="guest-list">' +
    draft.guests.map((email) => '<span class="guest-chip">' + escapeHtml(people[email]?.name || email) + '<button type="button" data-action="remove-guest" data-email="' + attr(email) + '" aria-label="Remove ' + attr(email) + '">×</button></span>').join("") +
    '</div><div class="guest-entry"><input name="guest" type="email" placeholder="Add a guest by email" aria-label="Guest email">' + action("add", "add-guest", ' type="button"') +
    '</div><div class="field-note">Try nora@studio.example or leo@studio.example.</div></div>' +
    '<label class="checkbox-field"><input type="checkbox" name="meet"' + (draft.meet ? " checked" : "") + '>Add Google Meet</label><div class="field-note" style="margin-top:-9px;margin-bottom:17px">A meeting link will be created when the event is saved.</div>' +
    field("Location", input("location", draft.location, "text", ' placeholder="A place, room, or address"')) +
    field("Description", '<textarea name="description" placeholder="What should everyone know?">' + escapeHtml(draft.description) + "</textarea>") +
    '<div class="form-grid">' + field("Show as", select("busy", [["busy", "Busy"], ["free", "Free"]], draft.busy)) + field("Visibility", select("visibility", [["default", "Calendar default"], ["private", "Private"], ["public", "Public"]], draft.visibility)) + "</div>" +
    field("Reminder", select("reminder", [["none", "No reminder"], ["5", "5 minutes before"], ["10", "10 minutes before"], ["30", "30 minutes before"], ["1440", "1 day before"]], draft.reminder)) +
    (draft.guests.length ? '<label class="checkbox-field"><input name="notify" type="checkbox"' + (draft.notify ? " checked" : "") + ">Notify guests about this event</label>" : "") +
    '<div id="form-status" class="form-status" role="alert"></div></form>';
  return panel("side", draft.id ? "edit event" : "new event", "side-panel joined", body,
    action(draft.guests.length && draft.notify ? "save & notify guests" : "save event", "save-event", "", true) + action("cancel", "cancel-edit") + '<span class="bar-spacer"></span><span class="small muted">local draft</span>');
}

function readEditor() {
  const form = document.getElementById("event-form");
  if (!form || !state.editor) return;
  const data = new FormData(form);
  for (const key of ["title", "calendar", "date", "endDate", "start", "end", "zone", "recurrence", "location", "description", "busy", "visibility", "reminder", "scope"]) {
    if (data.has(key)) state.editor[key] = String(data.get(key));
  }
  state.editor.allDay = data.has("allDay");
  state.editor.meet = data.has("meet");
  if (state.editor.guests.length) state.editor.notify = data.has("notify");
  const calendar = calendars.find((item) => item.id === state.editor.calendar);
  if (calendar) state.editor.organizer = accountFor(calendar).email;
}

function addGuest() {
  readEditor();
  const field = document.querySelector('[name="guest"]');
  const email = field.value.trim().toLowerCase();
  if (!email || !field.checkValidity()) { field.reportValidity(); return; }
  if (email === state.editor.organizer || state.editor.guests.includes(email)) { notify("This person is already included."); return; }
  state.editor.guests.push(email);
  state.selectedSlot = null;
  render({ keepScroll: true });
  document.querySelector('[name="guest"]')?.focus();
}

function saveEvent() {
  readEditor();
  const draft = state.editor;
  const form = document.getElementById("event-form");
  if (!form.reportValidity()) return;
  const error = document.getElementById("form-status");
  if (!draft.title.trim()) { error.textContent = "Give the event a title."; return; }
  if (draft.endDate < draft.date || (!draft.allDay && zonedInstant(draft.endDate, draft.end, draft.zone) <= zonedInstant(draft.date, draft.start, draft.zone))) {
    error.textContent = "The event must end after it starts."; error.scrollIntoView({ block: "nearest" }); return;
  }
  const calendar = calendars.find((item) => item.id === draft.calendar);
  if (!calendar || calendar.role === "reader" || !accountFor(calendar)?.calendar) { error.textContent = "Choose a connected, writable calendar."; return; }
  remember();
  draft.title = draft.title.trim();
  if (draft.id) {
    const original = events.find((event) => event.id === draft.id);
    const delta = dayDifference(draft.date, original.date);
    const length = dayDifference(draft.endDate, draft.date);
    events = events.map((event) => {
      const affected = event.id === original.id || (original.series && event.series === original.series &&
        (draft.scope === "all" || (draft.scope === "following" && event.date >= original.date)));
      if (!affected) return event;
      const day = addDays(event.date, delta);
      return { ...clone(draft), id: event.id, date: day, endDate: addDays(day, length), series: draft.recurrence === "none" ? null : event.series };
    });
  } else {
    draft.id = "local-" + Date.now();
    const series = draft.recurrence === "none" ? null : draft.id;
    const count = series ? 3 : 1;
    for (let index = 0; index < count; index++) {
      events.push({ ...clone(draft), id: index ? draft.id + "-" + index : draft.id, date: addDays(draft.date, index * 7), endDate: addDays(draft.endDate, index * 7), series });
    }
  }
  state.selectedEvent = draft.id;
  state.selectedDate = draft.date;
  state.side = "event";
  state.editor = null;
  state.availability = false;
  state.focus = "side";
  render();
  revealSide();
  notify("Saved to sample calendar · no invitations sent.", true);
}

function deletePanel() {
  const event = events.find((item) => item.id === state.selectedEvent);
  const body = '<div class="detail-content"><div class="eyebrow">Delete event</div><h2 class="delete-title">' + escapeHtml(event.title) + '</h2><p class="small secondary">' +
    prettyDate(event.date) + " · " + (event.allDay ? "all day" : event.start + "–" + event.end) + "</p>" +
    (event.series ? '<div class="detail-section"><h3>Delete which events?</h3>' + recurrenceScope("deleteScope", state.deleteScope) + "</div>" : "") +
    '<div class="detail-section"><p class="prose">' + (event.guests.length ? "Guests will be notified that this event is cancelled." : "This event will be removed from your calendar.") +
    '</p><p class="field-note">This preview only changes sample data. You can undo it.</p></div></div>';
  return panel("side", "delete event", "side-panel joined", body, action("delete event", "confirm-delete") + action("keep event", "keep-event"));
}

function sourcesPanel() {
  const body = '<div class="source-description">Choose what appears in this view. Account connections are shared with Mail.</div>' +
    accounts.filter((account) => account.provider === "Google").map((account) => '<section class="source-group"><h3>' + escapeHtml(account.label) + '</h3><p>' + escapeHtml(account.email) + "</p>" +
      (account.calendar ? calendars.filter((calendar) => calendar.account === account.id).map((calendar) => '<label class="source-row"><input type="checkbox" data-calendar="' + calendar.id + '"' + (calendar.selected ? " checked" : "") + "><span>" + calendar.mark + " " + escapeHtml(calendar.name) +
        '</span><span class="source-role">' + (calendar.role === "reader" ? "read only" : "can edit") + "</span></label>").join("") : '<div class="empty-services">Calendar is not enabled for this account.</div>') + "</section>").join("") +
    '<div class="source-description">Hiding a calendar changes this view. Finding a time still checks your connected personal calendars.</div>';
  return panel("side", "calendars", "side-panel joined", body, action("show all", "all-sources") + action("hide all", "no-sources") + link("manage accounts", "accounts"));
}

function availabilityPeople() {
  const owner = state.editor.organizer;
  return Array.from(new Set([owner, ...state.editor.guests]));
}

function busyFor(email, day) {
  // Different fixture dates deliberately have different responses.
  if (email === state.editor.organizer) {
    const ownIds = new Set(availableCalendars().filter((calendar) => calendar.role === "owner").map((calendar) => calendar.id));
    return events.filter((event) => ownIds.has(event.calendar) && event.busy === "busy" && event.response !== "declined" && event.id !== state.editor.id)
      .flatMap((event) => {
        const when = presented(event);
        if (when.date > day || when.endDate < day) return [];
        return [[event.allDay || when.date < day ? 0 : toMinutes(when.start), event.allDay || when.endDate > day ? 1440 : toMinutes(when.end)]];
      });
  }
  const offset = Math.abs(dayDifference(day, "2026-09-09")) % 3;
  if (email === "nora@studio.example") return [[540 + offset * 30, 615 + offset * 30], [780, 840], [930, 1020]];
  if (email === "leo@studio.example") return [[600, 660], [780 + offset * 15, 870 + offset * 15], [960, 1020]];
  return null;
}

function candidateSlots() {
  const day = state.availabilityDate;
  const attendees = availabilityPeople();
  const known = attendees.map((email) => busyFor(email, day)).filter((busy) => busy !== null);
  const bounds = state.hours === "work" ? [540, 1020] : [480, 1140];
  const found = [];
  for (let start = bounds[0]; start + state.duration <= bounds[1]; start += 15) {
    if (zonedInstant(day, clockTime(start), DISPLAY_ZONE) <= NOW) continue;
    if (known.every((busy) => busy.every(([from, to]) => start + state.duration <= from || start >= to))) {
      found.push({ day, start, end: start + state.duration });
    }
  }
  return found;
}

function availabilityPanel() {
  const attendees = availabilityPeople();
  const unknown = attendees.filter((email) => busyFor(email, state.availabilityDate) === null);
  const candidates = candidateSlots();
  if (!state.selectedSlot || !candidates.some((slot) => slot.start === state.selectedSlot.start && slot.day === state.selectedSlot.day && slot.end === state.selectedSlot.end)) {
    state.selectedSlot = candidates[0] || null;
  }
  const bounds = state.hours === "work" ? [540, 1020] : [480, 1140];
  const span = bounds[1] - bounds[0];
  const selected = state.selectedSlot;
  let body = '<div class="availability-content"><div class="availability-heading"><h2>Find a time together</h2><span class="small secondary">' + attendees.length + ' people</span></div><div class="availability-grid">' +
    field("Date", input("availabilityDate", state.availabilityDate, "date", ' id="availability-date" min="' + TODAY + '" required')) +
    field("Duration", '<select id="availability-duration" aria-label="Meeting duration">' + options([[15, "15 minutes"], [30, "30 minutes"], [45, "45 minutes"], [60, "1 hour"]], state.duration) + "</select>") +
    field("Look between", '<select id="availability-hours" aria-label="Scheduling hours">' + options([["work", "09:00–17:00"], ["wide", "08:00–19:00"]], state.hours) + "</select>") +
    '</div><p class="small secondary">Europe/Berlin · all times shown in your time zone</p><div class="availability-legend"><span><i class="key"></i>free</span><span><i class="key busy"></i>busy</span><span><i class="key unknown"></i>unknown</span></div>' +
    '<div class="schedule-row scale"><span></span><div class="schedule-times">' + Array.from({ length: 5 }, (_, index) => "<span>" + clockTime(Math.round(bounds[0] + span * index / 4)) + "</span>").join("") + "</div></div>";
  body += attendees.map((email) => {
    const busy = busyFor(email, state.availabilityDate);
    const isYou = email === state.editor.organizer;
    const title = isYou ? "You" : people[email]?.name || email.split("@")[0];
    const blocks = busy ? busy.map(([start, end]) => {
      const from = Math.max(start, bounds[0]), to = Math.min(end, bounds[1]);
      return to <= from ? "" : '<span class="busy-block" title="' + clockTime(start) + "–" + clockTime(end) + ' busy" style="left:' + (from - bounds[0]) / span * 100 + "%;width:" + (to - from) / span * 100 + '%"></span>';
    }).join("") : "";
    return '<div class="schedule-row"><div class="schedule-person">' + escapeHtml(title) + '<div class="small muted">' + (isYou ? "all own calendars" : busy ? "free / busy shared" : "not shared") +
      '</div></div><div class="time-track' + (!busy ? " unknown" : "") + '" aria-label="' + attr(title + (busy ? " availability" : ": availability unknown")) + '">' + blocks +
      (selected ? '<span class="proposed-block" style="left:' + (selected.start - bounds[0]) / span * 100 + "%;width:" + state.duration / span * 100 + '%"></span>' : "") + "</div></div>";
  }).join("");
  if (unknown.length) body += '<div class="unknown-explanation"><strong>' + unknown.length + " person" + (unknown.length > 1 ? "s have" : " has") + " unknown availability.</strong><br>" +
    unknown.map((email) => escapeHtml(people[email]?.name || email)).join(", ") + " " + (unknown.length > 1 ? "haven’t" : "hasn’t") + " shared a calendar with this account. These times work for the calendars we can check.</div>";
  body += '<section class="suggestions"><h3>' + (unknown.length ? "Suggested times · check with " + unknown.length + " guest" + (unknown.length > 1 ? "s" : "") : "Times that work for everyone") + "</h3>" +
    (candidates.length ? candidates.slice(0, 5).map((slot) => '<button class="suggestion' + (selected?.start === slot.start ? " chosen" : "") + '" data-action="pick-slot" data-start="' + slot.start + '"><span>' +
      clockTime(slot.start) + " — " + clockTime(slot.end) + "<small>" + prettyDate(slot.day, { weekday: "short", day: "numeric", month: "short" }) + " · " +
      (unknown.length ? attendees.length - unknown.length + "/" + attendees.length + " availability checked" : "everyone available") + '</small></span><span class="pick-indicator" aria-hidden="true"></span></button>').join("") :
      '<div class="notice">No shared opening in this range. Try another date, a shorter meeting, or a wider time window.</div>') +
    '<p class="avail-summary">' + (candidates.length > 5 ? "Showing the first 5 openings. " : "") + "Only busy blocks are needed; other people’s event details stay private.</p></section></div>";
  return panel("availability", "find a time", "availability-panel joined", body,
    action("use this time", "use-slot", selected ? "" : " disabled", true) + action("cancel", "close-availability") + '<span class="bar-spacer"></span><span class="small muted">sample availability</span>');
}

function accountsPanel() {
  const body = '<div class="accounts-intro"><h2>Your accounts</h2><p>Connect once. Choose the apps that use each account.</p></div><div class="accounts-head"><span>Account</span><span>Mail</span><span>Calendar</span></div>' +
    accounts.map((account) => '<button class="account-row' + (state.account === account.id ? " selected" : "") + '" data-action="account" data-id="' + attr(account.id) + '">' +
      '<span><span class="account-label">' + escapeHtml(account.label) + '</span><span class="account-email" style="display:block">' + escapeHtml(account.email) + '</span><span class="account-provider" style="display:block">' + account.provider + "</span></span>" +
      '<span class="service-state' + (!account.mail ? " off" : "") + '">' + (account.mail ? "✓ On" : "Off") + '</span><span class="service-state' + (!account.calendar ? " off" : "") + '">' +
      (account.calendar ? "✓ On" : account.provider === "Google" ? "Off" : "—") + "</span></button>").join("") +
    '<div class="account-footnote">This is a shared panel, available from the launcher, Mail, and Calendar.</div>';
  return panel("main", "accounts", "primary-panel", body, link("add account", "add-account") + '<span class="bar-spacer"></span><span class="small muted">' + accounts.length + " connections</span>");
}

function accountPanel() {
  const account = accounts.find((item) => item.id === state.account);
  if (!account) return panel("side", "account", "side-panel joined", '<div class="empty-state"><h2>Choose an account</h2></div>', link("add account", "add-account"));
  const body = '<div class="detail-content"><div class="eyebrow">' + account.provider + ' account</div><h2>' + escapeHtml(account.label) +
    '</h2><p class="secondary" style="font-size:12px;overflow-wrap:anywhere">' + escapeHtml(account.email) + '</p><div class="detail-section"><dl class="metadata"><dt>Connection</dt><dd><span class="status-dot"></span>Connected</dd><dt>Sign-in</dt><dd>' +
    (account.provider === "Google" ? "Google" : "App password") + "</dd></dl></div><div class=\"detail-section\"><h3>Used by</h3>" +
    '<div class="service-card"><div><h3>Mail</h3><p>' + (account.mail ? "Read and send email" : "Mail is turned off for this account") + "</p></div>" +
    action(account.mail ? "turn off" : "enable mail", "toggle-service", ' data-service="mail"') + "</div>" +
    (account.provider === "Google" ? '<div class="service-card"><div><h3>Calendar</h3><p>' + (account.calendar ? "Read and manage events" : "Permission is requested when enabled") +
      '</p><p class="field-note">' + (account.calendar ? calendars.filter((calendar) => calendar.account === account.id).length + " calendars connected" : "Mail can keep using this account") + "</p></div>" +
      action(account.calendar ? "turn off" : "enable calendar", "toggle-service", ' data-service="calendar"') + "</div>" : "") +
    '</div><div class="notice">Turning off one app keeps the account connected for the other apps.</div><div class="detail-section"><h3>Connection settings</h3><p class="small secondary">' +
    (account.provider === "Google" ? "Reconnect to refresh permissions for the apps you have enabled." : "Your existing IMAP and SMTP settings stay with Mail.") +
    '</p></div><div class="detail-bottom">Removing this connection disconnects every app using it on this device.</div></div>';
  return panel("side", "account · " + account.label, "side-panel joined", body, action("reconnect", "reconnect") + link("remove account", "remove-account", true));
}

function addAccountPanel() {
  const body = '<form id="account-form" class="form-content"><div class="eyebrow">Google account</div><h2>One connection, your apps.</h2>' +
    '<p class="small secondary" style="margin-bottom:22px">Choose which apps to connect. In the app, Google sign-in will open in your browser.</p>' +
    field("Label", input("label", "", "text", ' placeholder="e.g. Side project" required')) +
    field("Sample email", input("email", "", "email", ' placeholder="you@example.com" required')) +
    '<hr class="form-rule"><h3 style="margin-bottom:16px">Use this account with</h3><label class="checkbox-field"><input type="checkbox" name="mail" checked>Mail</label>' +
    '<label class="checkbox-field"><input type="checkbox" name="calendar" checked>Calendar</label><div class="notice">Preview only. Enter a sample address to try the connected state.</div><div id="account-status" class="form-status" role="alert"></div></form>';
  return panel("side", "add account", "side-panel joined", body, action("sign in with google ↗", "connect-account", "", true) + action("cancel", "cancel-account"));
}

function removeAccountPanel() {
  const account = accounts.find((item) => item.id === state.account);
  const body = '<div class="detail-content"><div class="eyebrow">Remove connection</div><h2>' + escapeHtml(account.email) + '</h2><p class="prose">This disconnects ' +
    [account.mail ? "Mail" : null, account.calendar ? "Calendar" : null].filter(Boolean).join(" and ") +
    ' from this account. Your email and events remain at the provider.</p><div class="detail-section"><p class="small secondary">In this preview, only the sample connection is removed.</p></div></div>';
  return panel("side", "remove account", "side-panel joined", body, action("remove connection", "confirm-remove-account") + action("keep account", "cancel-account"));
}

function agentPanel() {
  const body = '<div class="chat-content"><div class="chat-date">EXAMPLE CONVERSATION</div><div class="chat-context">↗ calendar · next 7 days · Europe/Berlin</div>' +
    '<div class="chat-message"><div class="chat-role">You</div><p class="prose">Find half an hour with Nora and Leo tomorrow afternoon. Add a Meet link.</p></div>' +
    '<div class="chat-message"><div class="chat-role">Agent</div><div class="tool-line">✓ Read your connected calendars</div><div class="tool-line">✓ Checked availability for 3 people</div><div class="tool-line">✓ Prepared an event draft</div>' +
    '<p class="prose" style="margin-top:17px">14:30–15:00 works for all three of you. I’ve prepared a draft on your Work calendar with Google Meet.</p>' +
    '<div class="draft-card"><div class="eyebrow">Event draft · not sent</div><h3>Catch-up with Nora & Leo</h3><p>Wed 9 Sep · 14:30–15:00<br>Work · 3 participants · Google Meet</p>' + link("review event", "agent-draft") +
    '</div></div><p class="small secondary">You can edit the draft before creating the event and notifying guests.</p></div>';
  return panel("side", "chat · calendar", "side-panel joined", body, link("review event", "agent-draft")) ;
}

const sceneNotes = {
  timeline: "Upcoming events across your accounts. Select a row to preview it beside the list.",
  month: "The same calendars and filters, in a month. A date opens its agenda; an event opens its details.",
  new: "A joined event draft. Changes in this prototype stay in memory; nothing is sent to Google.",
  availability: "Find a time continues to the right. Unknown availability stays distinct from free time.",
  accounts: "One shared account list for Mail and Calendar, with separate access for each app.",
  agent: "The agent prepares the same event draft you can edit. Calendar tools follow the app’s actions."
};

function render({ keepScroll = false } = {}) {
  const scroll = workspace.scrollLeft;
  const sideScroll = document.querySelector("#side .panel-body")?.scrollTop || 0;
  const mainScroll = document.querySelector("#main .panel-body")?.scrollTop || 0;
  let html = state.view === "accounts" ? accountsPanel() : state.view === "month" ? monthPanel() : timelinePanel();
  const sides = {
    event: eventPanel, day: dayPanel, editor: editorPanel, sources: sourcesPanel,
    account: accountPanel, "add-account": addAccountPanel, "remove-account": removeAccountPanel,
    delete: deletePanel, agent: agentPanel
  };
  if (state.side) html += sides[state.side]();
  if (state.availability && state.editor) html += availabilityPanel();
  workspace.innerHTML = html;
  document.querySelectorAll(".review-nav [data-scene]").forEach((button) => {
    if (button.dataset.scene === state.scene) button.setAttribute("aria-current", "page");
    else button.removeAttribute("aria-current");
  });
  document.getElementById("scene-note").textContent = sceneNotes[state.scene];
  // Bar/link controls inside forms are navigation, not implicit submit buttons.
  workspace.querySelectorAll("form button").forEach((button) => button.type = "button");
  if (keepScroll) {
    workspace.scrollLeft = scroll;
    if (document.querySelector("#side .panel-body")) document.querySelector("#side .panel-body").scrollTop = sideScroll;
    document.querySelector("#main .panel-body").scrollTop = mainScroll;
  }
}

function revealSide() {
  const side = document.getElementById("side");
  if (side && (window.innerWidth <= 1050 || state.availability)) side.scrollIntoView({ block: "nearest", inline: "end", behavior: "auto" });
}

function renderResults() {
  document.querySelectorAll(".quick-filter[data-tag]").forEach((button) =>
    button.setAttribute("aria-pressed", String(state.query.split(/\s+/).includes(button.dataset.tag))));
  if (state.view === "month") {
    const filter = document.getElementById("event-filter");
    const caret = filter.selectionStart;
    render({ keepScroll: true });
    const next = document.getElementById("event-filter");
    next.focus();
    try { next.setSelectionRange(caret, caret); } catch (_) { /* Some search inputs do not expose selection. */ }
  } else {
    const list = matchingEvents();
    document.getElementById("agenda").innerHTML = agendaHtml(list);
    document.getElementById("result-count").textContent = list.length + " upcoming events";
  }
}

function startAvailability() {
  if (!state.editor && !editSelected()) newEditor(addDays(TODAY, 1));
  if (!state.editor) return;
  readEditor();
  if (!state.editor.date || !state.editor.endDate || !state.editor.start || !state.editor.end) {
    notify("Choose a date and start/end times first."); return;
  }
  if (state.editor.allDay) { notify("Choose a timed event before finding a meeting time."); return; }
  state.availabilityDate = state.editor.date < TODAY ? TODAY : state.editor.date;
  state.duration = [15, 30, 45, 60].includes(toMinutes(state.editor.end) - toMinutes(state.editor.start)) ? toMinutes(state.editor.end) - toMinutes(state.editor.start) : 30;
  state.availability = true;
  state.selectedSlot = null;
  state.focus = "availability";
  render();
  document.getElementById("availability").scrollIntoView({ block: "nearest", inline: "end", behavior: "auto" });
}

function setScene(scene) {
  readEditor();
  state.scene = scene;
  state.availability = false;
  state.editor = null;
  state.focus = "main";
  if (scene === "accounts") { state.view = "accounts"; state.side = "account"; }
  else if (scene === "month") { state.view = "month"; state.side = "day"; }
  else {
    state.view = "timeline";
    state.side = scene === "agent" ? "agent" : "event";
    if (!events.some((event) => event.id === state.selectedEvent)) state.selectedEvent = matchingEvents()[0]?.id;
    if (scene === "new") newEditor();
    if (scene === "availability") {
      newEditor(addDays(TODAY, 1));
      if (state.editor) {
        Object.assign(state.editor, { title: "Project catch-up", guests: ["nora@studio.example", "leo@studio.example", "ava@partner.example"], meet: true });
        state.availabilityDate = "2026-09-09";
        state.duration = 30;
        state.selectedSlot = null;
        state.availability = true;
        state.focus = "availability";
      }
    }
  }
  render();
  workspace.scrollLeft = 0;
  if (state.availability) document.getElementById("availability").scrollIntoView({ block: "nearest", inline: "end", behavior: "auto" });
  else if (scene === "new" || scene === "agent") revealSide();
}

document.addEventListener("click", (event) => {
  const scene = event.target.closest("[data-scene]");
  if (scene) { setScene(scene.dataset.scene); return; }
  const control = event.target.closest("[data-action]");
  if (!control || control.disabled) return;
  event.preventDefault();
  const name = control.dataset.action;
  switch (name) {
    case "reset": reset(); document.getElementById("toast").classList.remove("visible"); return;
    case "undo":
      if (!undoSnapshot) return;
      ({ events, accounts, calendars } = clone(undoSnapshot));
      undoSnapshot = null; state.side = state.view === "accounts" ? "account" : "event";
      render(); notify("Sample change undone."); return;
    case "timeline": setScene("timeline"); return;
    case "month": setScene("month"); return;
    case "accounts": setScene("accounts"); return;
    case "agent": state.scene = "agent"; state.side = "agent"; state.availability = false; state.focus = "side"; break;
    case "sync": notify("Sample calendars refreshed."); return;
    case "join-meet": notify("Preview only · no meeting is opened."); return;
    case "event":
      state.selectedEvent = control.dataset.id; state.side = "event";
      state.editor = null; state.availability = false; state.focus = window.innerWidth <= 650 ? "side" : "main"; break;
    case "day": state.selectedDate = control.dataset.date; state.side = "day"; state.focus = "main"; break;
    case "today": state.month = TODAY.slice(0, 7) + "-01"; state.selectedDate = TODAY; state.side = "day"; break;
    case "prev-month":
    case "next-month": {
      const date = dateAtNoon(state.month); date.setUTCMonth(date.getUTCMonth() + (name === "next-month" ? 1 : -1));
      state.month = date.toISOString().slice(0, 10); state.selectedDate = state.month; state.side = "day"; break;
    }
    case "filter-tag": {
      const tags = state.query.split(/\s+/).filter(Boolean), tag = control.dataset.tag;
      state.query = (tags.includes(tag) ? tags.filter((item) => item !== tag) : tags.concat(tag)).join(" ");
      render({ keepScroll: true }); return;
    }
    case "clear-filter": state.query = ""; render({ keepScroll: true }); return;
    case "sources": readEditor(); state.side = "sources"; state.availability = false; state.focus = "side"; break;
    case "all-sources":
    case "no-sources": calendars.forEach((calendar) => calendar.selected = name === "all-sources"); render({ keepScroll: true }); return;
    case "new": state.scene = "new"; newEditor(); break;
    case "new-on-day": state.scene = "new"; newEditor(state.selectedDate); break;
    case "edit": editSelected(); break;
    case "duplicate": {
      const selected = events.find((item) => item.id === state.selectedEvent);
      newEditor(selected.date);
      if (state.editor) state.editor = { ...state.editor, title: selected.title + " (copy)", description: selected.description, start: selected.start, end: selected.end, date: selected.date, endDate: selected.endDate, allDay: selected.allDay, zone: selected.zone, meet: selected.meet, location: selected.location };
      break;
    }
    case "cancel-edit": state.side = events.some((item) => item.id === state.selectedEvent) ? "event" : null; state.editor = null; state.availability = false; break;
    case "save-event": saveEvent(); return;
    case "add-guest": addGuest(); return;
    case "remove-guest": readEditor(); state.editor.guests = state.editor.guests.filter((email) => email !== control.dataset.email); state.selectedSlot = null; render({ keepScroll: true }); return;
    case "find-time": startAvailability(); return;
    case "close-availability": state.availability = false; state.focus = "side"; break;
    case "pick-slot":
      state.selectedSlot = candidateSlots().find((slot) => slot.start === Number(control.dataset.start));
      render({ keepScroll: true }); return;
    case "use-slot":
      if (!state.selectedSlot) return;
      readEditor();
      Object.assign(state.editor, { date: state.selectedSlot.day, endDate: state.selectedSlot.day, start: clockTime(state.selectedSlot.start), end: clockTime(state.selectedSlot.end), zone: DISPLAY_ZONE });
      state.availability = false; state.focus = "side";
      render(); revealSide(); notify("Time added to the draft."); return;
    case "delete": state.deleteScope = "this"; state.side = "delete"; state.focus = "side"; break;
    case "keep-event": state.side = "event"; break;
    case "confirm-delete": {
      const selected = events.find((item) => item.id === state.selectedEvent);
      if (!isWritable(selected)) return;
      remember();
      events = events.filter((item) => item.id !== selected.id && !(selected.series && item.series === selected.series &&
        (state.deleteScope === "all" || (state.deleteScope === "following" && item.date >= selected.date))));
      state.side = "event"; render(); notify("Removed from sample calendar · no cancellations sent.", true); return;
    }
    case "rsvp": {
      const selected = events.find((item) => item.id === state.selectedEvent);
      remember(); selected.response = control.dataset.response; render();
      notify("Sample response updated · no reply sent.", true); return;
    }
    case "account": state.account = control.dataset.id; state.side = "account"; break;
    case "add-account": state.side = "add-account"; state.focus = "side"; break;
    case "cancel-account": state.side = "account"; break;
    case "toggle-service": {
      const account = accounts.find((item) => item.id === state.account);
      remember(); account[control.dataset.service] = !account[control.dataset.service];
      render({ keepScroll: true });
      notify(account[control.dataset.service] ? "Sample access enabled. The app will request Google permission here." : "App disconnected from sample account.", true);
      return;
    }
    case "reconnect": notify("Preview only · Google would reopen sign-in for the enabled apps."); return;
    case "remove-account": state.side = "remove-account"; state.focus = "side"; break;
    case "confirm-remove-account":
      remember(); accounts = accounts.filter((account) => account.id !== state.account);
      calendars = calendars.filter((calendar) => calendar.account !== state.account);
      events = events.filter((item) => calendars.some((calendar) => calendar.id === item.calendar));
      state.account = accounts[0]?.id; state.side = "account"; render(); notify("Sample account removed.", true); return;
    case "connect-account": {
      const form = document.getElementById("account-form");
      if (!form.reportValidity()) return;
      const data = new FormData(form);
      const email = String(data.get("email")).trim().toLowerCase();
      const label = String(data.get("label")).trim();
      if (!label || (!data.has("mail") && !data.has("calendar"))) { document.getElementById("account-status").textContent = "Add a label and choose at least one app."; return; }
      if (accounts.some((account) => account.email === email)) { document.getElementById("account-status").textContent = "This account is already connected. Choose it in the list to enable another app."; return; }
      remember();
      const id = "account-" + Date.now();
      accounts.push({ id, email, label, provider: "Google", mail: data.has("mail"), calendar: data.has("calendar") });
      calendars.push({ id, account: id, name: label, mark: "◇", role: "owner", selected: true });
      state.account = id; state.side = "account"; render(); notify("Sample account connected · no Google sign-in performed.", true); return;
    }
    case "agent-draft":
      newEditor("2026-09-09");
      if (state.editor) Object.assign(state.editor, { title: "Catch-up with Nora & Leo", start: "14:30", end: "15:00", guests: ["nora@studio.example", "leo@studio.example"], meet: true });
      state.scene = "new"; break;
    case "close":
      if (control.dataset.panel === "availability") { state.availability = false; state.focus = "side"; }
      else if (control.dataset.panel === "side") { state.side = null; state.availability = false; state.editor = null; state.focus = "main"; }
      else { workspace.innerHTML = '<div class="empty-state"><h2>Workspace cleared</h2><p>Choose a scene above to open Calendar again.</p></div>'; return; }
      break;
    default: return;
  }
  render({ keepScroll: true });
  if (name !== "close") revealSide();
});

workspace.addEventListener("input", (event) => {
  if (event.target.id === "event-filter") { state.query = event.target.value; renderResults(); }
  else if (event.target.closest("#event-form") && event.target.name !== "guest") readEditor();
});

workspace.addEventListener("change", (event) => {
  const target = event.target;
  if (target.id === "range") { state.range = Number(target.value); renderResults(); }
  else if (target.dataset.calendar) {
    calendars.find((calendar) => calendar.id === target.dataset.calendar).selected = target.checked;
    render({ keepScroll: true });
  } else if (target.name === "deleteScope") state.deleteScope = target.value;
  else if (["availability-date", "availability-duration", "availability-hours"].includes(target.id)) {
    if (target.id === "availability-date") {
      if (!target.value || target.value < TODAY) { target.value = state.availabilityDate; notify("Choose today or a later date."); return; }
      state.availabilityDate = target.value;
    }
    if (target.id === "availability-duration") state.duration = Number(target.value);
    if (target.id === "availability-hours") state.hours = target.value;
    state.selectedSlot = null; render({ keepScroll: true });
  } else if (target.closest("#event-form")) {
    readEditor();
    if (["allDay", "recurrence", "notify", "calendar"].includes(target.name)) render({ keepScroll: true });
  }
});

workspace.addEventListener("submit", (event) => {
  event.preventDefault();
  if (event.target.id === "event-form") saveEvent();
});

workspace.addEventListener("focusin", (event) => {
  const focused = event.target.closest(".panel");
  if (!focused || event.target.matches(".event-row, .month-event")) return;
  state.focus = focused.id;
  workspace.querySelectorAll(".panel").forEach((element) => element.classList.toggle("focused", element === focused));
});

document.addEventListener("keydown", (event) => {
  const inField = event.target.matches("input, textarea, select");
  if (event.key === "Enter" && event.target.name === "guest") { event.preventDefault(); addGuest(); return; }
  if (event.key === "/" && !inField) { event.preventDefault(); document.getElementById("event-filter")?.focus(); return; }
  if (event.key === "Escape" && inField) { event.target.blur(); return; }
  if ((event.key === "ArrowDown" || event.key === "ArrowUp") && (!inField || event.target.id === "event-filter")) {
    const rows = Array.from(document.querySelectorAll("#agenda .event-row"));
    if (!rows.length) return;
    event.preventDefault();
    const index = rows.findIndex((row) => row.dataset.id === state.selectedEvent);
    const next = rows[Math.min(rows.length - 1, Math.max(0, index + (event.key === "ArrowDown" ? 1 : -1)))];
    next.click();
    const row = document.querySelector('#agenda [data-id="' + next.dataset.id + '"]');
    row?.focus({ preventScroll: true });
    row?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }
});

reset();
