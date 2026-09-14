# Fluent

Fluent is a language course that lives in the store. The tutor is the
[agent](./agents.md); the learner plays. A **lesson** is authored whole by
the tutor between two sittings — the arc, the exercises, the model answers
— and played without waiting on a model; a **card** is a word on a
flashcard, reviewed on its own schedule; every grade is a **review** row,
which is what the schedule and the tutor both read. The course came from a
Claude Code plugin with a Mac player, an iPhone player and six JSON
notebooks in an iCloud folder; here the notebooks are tables, the two
players are panels, and the device split is gone — the review on the phone
is the desk's panel clamped to a smaller grid.

## The words

- The **learner** is the one person; the row says the languages, the
  level, the goal, the daily minutes and the streak.
- A **lesson** is one pre-authored sitting — the original's *session*,
  renamed because *session* is the workspace's word in this book. It has an
  arc of sections (warmup, review, new, set piece, cooldown) and a list of
  **exercises**, each of one of six kinds: choose, cloze, translate, free
  write, listen, read.
- An exercise is **closed** — graded on the spot against its accepted
  answers — or **self-check**: it shows the model answer, takes the
  learner's own 0–5 grade, and the tutor's grade lands later and overrules.
- An **item** is anything on a schedule: a word, a grammar rule, an error
  pattern. Its SM-2 state says when it comes due.
- A **card** is the flashcard behind a word: front, back, example, what to
  speak, a note.
- A **review** is one grade of one item, whenever and wherever it was
  given. The schedule is what an item's reviews add up to.
- A **topic** is one rule of the grammar reference the tutor keeps.
- The **tutor** is the agent app, given the course's tools.

## Tags and roots

| Tag | Argument | What it shows |
|---|---|---|
| `fluent` | none | the desk: the greeting and the streak, the shelf, three tiles |
| `lesson` | a lesson id | the player, one exercise at a time; the summary once the last is answered |
| `review` | none | the cards due today, one at a time |
| `cards` | none, or a filter | the deck, soonest due first |
| `card` | an item id | one card whole, with its schedule and every grade |
| `lookup` | a term | one word: what it means, and which of the deck, the cache and the tutor said so |
| `grammar` | none, or a filter | the topics, by category |
| `topic` | a topic id | one rule: its tables, examples, tips, the learner's notes |
| `lessons` | none, or a filter | every lesson played, newest first |
| `progress` | none | the streak, eight weeks of days, mastery, accuracy, errors, words |
| `fluent-setup` | none | the learner's languages, level, goal and minutes |
| `fluent-import` | none | the original course's folder, read into the store |

The roots the launcher offers: **fluent**, **review cards**, **cards**,
**grammar**, **progress**, **fluent setup**, **fluent import**. History is
a link from the desk and from progress; a lesson and a topic are reached
from their lists.

## The desk

The `fluent` panel is where a day starts. The greeting is the hour's
(*Guten Tag, Andrey!*) and the line under it is the streak: alive, paused,
or not begun. A store with no learner row says so in the shelf box and
offers **set up** (`e`), which opens the `fluent-setup` form joined to the
desk: name, native and target language, level and goal on the A1–C2
ladder, daily minutes, and **save** (`s`) — one undoable write of the
learner row. Nothing starts or builds until the course has a learner. Then
the shelf, a hairline box, in one of four states:

- **ready** — the lesson's title, its focus tags, and *9 exercises · ~20
  min · 3 reviews woven in · for today*; **start** on the bar. A lesson left
  half-way says *started*, and the bar says **continue**.
- **building** — the tutor is at work on the next lesson; **tutor** (`o`)
  on the bar goes to its chat, and the note under the title reads the run's
  state: grading and authoring, waiting for the chat to be open, or failed
  and wanting a retry.
- **stale** — built for a day more than three days gone; **build fresh** and
  **play anyway** on the bar. Building fresh leaves the stale lesson where it
  stands — it was never played, and a row saying otherwise would lie to the
  history — and puts today's placeholder on the shelf above it.
- **empty** — no lesson at all; **build**, which calls the tutor.

Under it, three tiles: what is due today (cards, with the new ones and the
grammar items counted), the accuracy of the last five lessons with the
trend in a word, and the words learned with this week's arrivals. The rest
of the bar goes to the course: **review n**, **cards**, **grammar**,
**progress**, **history**.

## The lesson

A `lesson` panel plays one exercise at a time. The caption says the section
and the kind (`REVIEW · CLOZE`), the count says *3 OF 9*, and a hairline
under them fills as the lesson goes. The prompt is the hero, in the prose
face; a set piece pins its passage in a box above the question, and a
listening withholds the prompt behind *Was hast du gehört?* until the
answer, with **play** on the bar and `space` on the keys.

- A **choice** is a row with the digit in a box at its left. The digit
  answers it and checks it at once; the arrows walk the rows and `enter`
  takes the walked one; a press does the same.
- A **gap** or a **translation** is a field, the caret already in it;
  `enter` checks, **check** on the bar does the same for a pointer.
- A **free write** is an editor; `enter` checks and `shift+enter` breaks a
  line, as the agent's composer has it.
- **hint** unfolds the next hint under the prompt, one at a time, and the
  count of hints shown goes into the record.

A closed answer is graded on the spot. The verdict sits in a wash under
the answer: **Richtig!**, **Fast!** for the same letters with a case or an
umlaut slip, **Nicht ganz.** with *you: der Gebühr* over *→ die Gebühr* and
the explanation. In a choice the right row is marked *answer* and the picked
wrong one *yours*. **next** or `enter` goes on. A self-check answer that is
the model answer, or one of the accepted ones, is simply right; any other
shows the model answer in a box beside what was written, and the bar is the
grade pad: **0 blank · 1 wrong · 2 almost · 3 hard · 4 good · 5 easy**, the
digit in each label the plain key that fires it. **ask** on the bar in every
feedback state, and **end** while answering, which closes the lesson where
it stands.

**Looking a word up.** Everything on the stage in the course's own language
— the prompt, the passage, a listening's transcript, the model answer, the
variants also accepted, the explanation, the hints — is a selectable run
and not a label. A selection of at most three words and forty characters,
with at least one letter in it, puts **lookup** (`k`) on the bar, which
opens a `lookup` panel joined to the lesson. Four words, a row of a cloze's
underscores, nothing at all: the bar simply does not offer it. The
selection is the keyboard's — the run that holds it holds the selection,
and a caret back in the answer field ends it.

A lookup resolves the word in the order that costs least. **The deck**: a
card whose front is the word, with or without an article on either side and
case folded, which is instant and offline. **The cache**: `fluent_lookup`,
what the tutor answered the last time anybody asked. **The tutor**: one
question with no chat behind it, asked on a worker while the panel says
*asking the tutor…*. The panel says which of the three answered, because a
word out of one's own deck and a word out of a model are worth different
amounts. **play** (`y`) speaks it; **card** (`c`) goes to the flashcard
behind it where the deck has one; **add** (`d`) puts it there where it does
not — an item and a card in one undoable action, the way the tutor's own
cards land, after which the bar offers the card instead. A word costs one
question ever: what the tutor says goes into the cache, which is a plain
write and not an action, because a cache is nobody's decision.

**The grade that comes back.** A self-check answer that is not the model
answer goes to the tutor the moment it is written — one question, the same
judgement the brief spells out, one JSON object back — and the learner
waits for none of it: the model answer is already up and the grade pad is
already on the bar. The verdict lands a few seconds later as an ordinary
[`fluent.grade`](#tools) write, rewriting the reviews that answer filed,
and says one quiet line: *tutor checked Q9 — agrees (4/5)*, *tutor suggests
3/5 for Q9 — you said 4*, or *tutor checked Q9 — 4/5* where nobody has
graded it yet. While the exercise is still up, the tutor's line and its
correction are under the model answer's box. An exercise that already
carries a tutor's grade is never written over, and a question that fails
says *the tutor could not check Q9 — the lesson's end will* — a quiet line
too, never an error, because nothing the learner did went wrong. The
lesson's end grades whatever this did not reach.

Every answer is written the moment it is given — the answer, the result,
the grades, the hints shown, the seconds — and every closed grade and every
self-grade files one review per item the exercise names. Closing the panel
loses nothing: reopening it resumes at the first exercise not done — at its
grade pad where it was answered and never graded — or at the summary, with
**finish** on the bar, where every answer is in and the row is still open.
One `cmd+z` takes an answer back with its reviews, and the player follows
the rows on every draw: an answer undone under its feedback is that
exercise to answer again, a self-grade undone is its grade pad again.

After the last exercise the same panel is the **summary**: *Geschafft!*,
*8/9 first try · 89% · 21 min*, the streak line, the calibration of self
against tutor where both exist, the tutor's notes, and one row per
correction. Finishing stamps the lesson's accuracy and minutes on its row —
the minutes are the seconds the exercises themselves took, so a lesson
closed overnight is not a night's study — feeds the streak, puts a
**building** placeholder on the shelf for tomorrow, and calls the tutor. A
tutor's grade that lands after the finish stamps the accuracy again.

## The tutor

There is no tutor panel of this app's own. The tutor is a chat of the
[agent app](./agents.md) opened by this one, joined to the panel it is
about, with the panel as a chip and the course's brief as its first turn —
who the learner is, how to grade, how to author a lesson, what to keep up.
Finishing a lesson opens the chat beside the summary: the tutor reads the
lesson with `fluent.lesson`, grades with `fluent.grade` each free answer
that was not already graded as it was written, reads `fluent.due`, and puts
tomorrow's lesson on the shelf with one `fluent.author` — cards for the new
words, topics for the rules it touched, a note per stumble. **build** on the
desk opens the same chat for today's lesson; on an empty course the brief
says to start gently. **ask** on a
lesson is the workspace's `cmd+shift+a`: a chat carrying the lesson as a
chip, for a question about it.

A run's tool calls execute while its chat is shown; a chat closed mid-run
pauses at the next call, which is what the desk's *waiting for the chat*
says. Nothing runs unasked.

Two questions have no chat at all: the grade of a self-check answer as it
is written, and the word a selection looks up. Neither wants a
conversation — there is nothing to say back to, nothing to call, and
nothing a person would open a panel to read — so each is a system line and
one question through the agent's `ask_once`, sent on a worker and answered
where it was asked. They are requests like any other: the effects log shows
each as *ask the model once: …*.

## The review

A `review` panel is the flashcards due today, the queue taken when the
panel opens so a grade does not move it under the learner. The caption says
*CARD 4 OF 18* and *3 NEW · 15 REVIEW*. The card is a box with the word
alone; **show**, `enter`, `space` or a press turns it: the meaning, the
example in italics, the note. Then the grade pad on the bar and **play**
beside it. A grade files a review and moves the card's schedule, one undo
apiece; the next card is up at once, and a grade undone puts its card back
up — the sitting is read off the grades filed since the panel opened, so
the score and the place follow them. After the last, *Alles erledigt!*, the
count, the right ones and the minutes. Nothing due says so and names the
weekday the next card comes.

On a phone the box is display and everything that acts is on the bar at the
foot, which is where a thumb is; the bar wraps the six grades over two rows.

## Speech

**play** speaks a card's or an exercise's audio text through the `Speech`
capability the kernel owns and the platform supplies — AVSpeechSynthesizer
on macOS, TextToSpeech on android — in the learner's target language. A
scripted run and a library mount have the fake, which records what it was
told; a world with no speech says what it would have said.

## The lists and the cards

`cards`, `grammar` and `lessons` are [rich tables](./richtable.md). The
deck's row is the word over its meaning, with when it comes due and its
mastery at the right; `@new`, `@weak`, `@learned` and `@mastery` narrow it.
The grammar's rows are grouped under their category in the original's
order — Fälle, Präpositionen, Adjektive, Verben, Satzbau, Pronomen, Nomen,
Sonstiges — with the level and the mastery stamp at the right; `@level` and
`@category` narrow it. The history's rows are the title and the day, the
accuracy, the minutes and the focus under it; `@date` and `@accuracy`
narrow it. Every cursor previews — a card, a topic, a lesson's summary — by
the shell's rule.

`card` is one flashcard whole: the word, the meaning, the example, the
note, then *ease 2.36 · 3 reviews · due tomorrow · mastery 2/5* with the
mastery as five cells, and every grade it was given with the day and where
it came from. `topic` is one rule: the title, *A1 · Fälle · mastery 3/5*,
the lessons it was introduced and last practiced in, its sections in order —
a paragraph, a table laid out in the mono face, examples with their notes, a
tip in a box — then the learner's own stumbles on this rule, and dotted
links to related topics that replace the panel.

## Progress

`progress` is read off the rows and edits nothing: the streak and whether
today keeps it; the last 56 days as eight rows of seven cells, each filled
by the minutes studied against the daily goal; mastery per skill as five
cells with the accuracy and the lesson count; the last ten lessons'
accuracy as bars; the three error patterns seen most often; the words
learned and due. **history** on the bar.

## The store

Every table is prefixed `fluent_`. Dates are days: unix seconds at 00:00
UTC, so *due today* is one comparison and `sqlite3` reads them with
`datetime(due, 'unixepoch')`.

| Table | What a row is |
|---|---|
| `fluent_learner` | the one learner: name, native and target language, level and goal, daily minutes, streak, when it was last fed |
| `fluent_item` | anything on a schedule, by slug: kind (vocab, grammar, error), one line of content, when it was created, the `base` its replay starts from, and this device's derived SM-2 cache |
| `fluent_card` | the flashcard behind a vocab item |
| `fluent_lookup` | one word the tutor was once asked the meaning of — a cache, keyed by the dictionary form, and the one table here that travels nowhere |
| `fluent_review` | one grade: item, instant, quality, device, and the lesson uid and exercise seq it came from where it did |
| `fluent_lesson` | one sitting, named across devices by its `uid`: title, day, focus, status (building, ready, done), when it was generated, started and ended, its accuracy, minutes and the tutor's notes; the tutor's chat, locally |
| `fluent_exercise` | one exercise of a lesson, keyed by the lesson's uid and its seq: the content, then what happened |
| `fluent_topic` | one rule, with its sections as JSON, the uids of the lessons it was introduced and last practiced in, and a rank kept from its category |
| `fluent_topic_note` | one of the learner's stumbles on a topic |
| `fluent_mistake` | an error pattern with a wrong → right example |
| `fluent_skill` | mastery and accuracy per skill |

### What replicates, and what is derived

Every decision travels between the learner's devices through
[device sync](./device-sync.md): the learner row, the items' content, the
cards, every review, the lessons and their exercises with the answers, the
topics and the notes, the mistakes and the skills. Each is keyed by what
names its row on every device — a slug, a uid, the instant a grade was
given — and every other column has a default, because a row another device
made arrives one cell at a time; triggers number the local lesson ids from
the uids in whichever order the rows land.

An item's schedule — ease, interval, reps, due, mastery — is not a decision
and does not travel. It is replayed from the item's reviews: on every grade,
on undo, and on the app's poll when sync has brought grades in or taken
them away, so two devices grading one card in one day agree by adding their
grades up rather than by overwriting each other. Two grades in one instant
are ordered by the device that gave them, never by a local row id. The
tutor's grade on a self-check answer rewrites the reviews that answer filed
and replays again. The SM-2 step is the original's, bit for bit, and the
parity fixture it generated from its Python reference is the test.

Undo and redo name the same rows: a lesson redone comes back with the
exercise ids it had, a grade redone under its own review id, and a finish
redone puts back the very placeholder it made, so an action undone and
redone after another still finds what it is about.

The replay starts from the item's `base`, which does travel: for an item
made here, the day it was first due, so an item every grade of which has
been undone stands where it began; for a migrated item whose notebook
carried a schedule without its history, that schedule, so the first grade
given here carries the progress on rather than starting the word over.

The lookup cache is not a decision either, and travels nowhere: a device
that has never looked a word up simply asks, and a device that lost the
table asks again. A word worth keeping is a card, and **add** is how it
becomes one.

### Tools

| Tool | Writes | What it does |
|---|---|---|
| `fluent.due` | no | every item due today with the card behind it |
| `fluent.lesson` | no | one lesson whole with every answer, grade and note; the newest finished one when unnamed |
| `fluent.grade` | yes | the tutor's 0–5 on a self-check answer, one line of feedback, the corrected text; rewrites the grades the answer filed; one undo |
| `fluent.author` | yes | a whole lesson onto the shelf with the cards, topics, notes and mistakes it brings; replaces the building placeholder; one undo removes it all |

None asks first: a grade and a lesson are one `cmd+z` away.
`App::describe` says the tables in prose and which columns are derived, so
the model prefers the tools to `sql.write`.

## Migration

`fluent-import` is a form over the original course's folder — its field
starts on the iCloud folder the plugin used — and **import** (`m`) reads
the six notebooks, the deck, the grammar reference, the next session and
the per-device review logs into the store as one undoable action: items
with their histories as reviews, replayed into a schedule (an item with no
history keeps the state the notebook gave it); cards, topics and notes; the
sessions played as finished lessons, named `import-<session>` so the
grammar's references resolve; the session on the original's shelf as
today's, its set piece's passage put on every question it belongs to. The
read is a worker's, the write is the UI thread's, and the status line says
what was added and what the course already had. Importing the same folder
twice adds nothing.

## Not done, on purpose

- **Confetti, the streak flame, the emoji whitelist.** A finished lesson
  says *Geschafft!* in the prose face and nothing moves.
- **The pause overlay.** Closing the panel is the pause.
- **The key-hint bar.** The digit is in the choice's box and the letters
  are bold on the bar.
- **A tutor sidebar.** The agent's chat is the tutor, joined like any panel.
- **Colour for right and wrong.** A word says it; red is for errors of the
  machine.
- **The lookup card that hangs from the selection.** The original drew the
  answer in an overlay under the swept word; here it is a panel joined to
  the lesson, which is what this workspace does with a thing worth reading.
