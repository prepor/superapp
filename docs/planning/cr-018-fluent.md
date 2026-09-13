# CR-018 · Fluent: a language course in the store

**Status:** draft, round one. The surface is drawn in the panels library over
a demo course; the lessons and the deck are real rows and every grade is an
undoable write, but no tutor runs yet. Written as the book's *Fluent* chapter
should read once the whole change has landed, with the phases and the open
decisions at the end.

## Why

Fluent is a German course that lives in `~/cloud/fluent`: a Claude Code
plugin that grades and authors, a macOS app that plays a pre-authored
session, an iPhone app that turns flashcards, and six JSON notebooks in an
iCloud folder that all three read, with one Python pipeline as the only
writer. It works, and it is three programs and a folder.

Superapp already has the parts. It has one store every app writes through
and one agent that reads every table and acts through the apps' own tools.
It has panels, joins and a bar, on a Mac and on a phone from one codebase.
So the course comes over as an app: the notebooks become tables, the tutor
becomes the agent app with four tools, the two players become panels, and
the device split disappears — a card review on the Fold is the same panel
as on the desk, clamped to its grid.

The original is the reference for what is shown and what happens; the look
is the book's. Its ink-on-paper palette, the terracotta accent, the serif
German, the confetti, the key-hint bar and the tutor sidebar do not come
over. Its pedagogy does, whole.

## The words

- The **learner** is the one person; the row says the languages, the level,
  the goal and the streak.
- A **lesson** is one pre-authored sitting — the original's *session*,
  renamed because *session* is the workspace's word in this book. It has an
  arc of sections (warmup, review, new, set piece, cooldown) and a list of
  **exercises**, each of one of six kinds: choose (mcq), cloze, translate,
  free write, listen, read.
- An exercise is **closed** — graded on the spot against its accepted
  answers — or **self-check**: it shows the model answer, takes the learner's
  own 0–5 grade, and the tutor's grade lands later and overrules.
- An **item** is anything on a schedule: a word, a grammar rule, an error
  pattern. Its SM-2 state says when it comes due.
- A **card** is the flashcard behind a word: front, back, example, what to
  speak, a note.
- A **review** is one grade of one item, whenever and wherever it was given.
  Every grade in the app is one; the schedule is what they add up to.
- A **topic** is one rule of the grammar reference the tutor keeps.
- The **tutor** is the agent app, given the course's tools.

## The app

The fluent app is a desk, a player, a card review, three lists, two cards
and a progress page. It registers nine panel kinds, its own schema ladder, a
demo course, and four tools.

### Tags and roots

| Tag | Argument | What it shows |
|---|---|---|
| `fluent` | none | the desk: the greeting and the streak, the shelf, three tiles |
| `lesson` | a lesson id | the player, one exercise at a time; the summary once the last is answered |
| `review` | none | the cards due today, one at a time |
| `cards` | none, or a filter | the deck, soonest due first |
| `card` | an item id | one card whole, with its schedule and every grade |
| `grammar` | none, or a filter | the topics, by category |
| `topic` | a topic id | one rule: its tables, examples, tips, the learner's notes |
| `lessons` | none, or a filter | every lesson played, newest first |
| `progress` | none | the streak, eight weeks of days, mastery, accuracy, errors, words |

The roots the launcher offers: **fluent**, **review cards**, **cards**,
**grammar**, **progress**. History is a link from the desk and from
progress; a lesson and a topic are reached from their lists.

### The desk

The `fluent` panel is where a day starts. The greeting is the hour's
(*Guten Tag, Andrey!*) and the line under it is the streak: alive, paused,
or not begun. Then the shelf, a hairline box, in one of four states:

- **ready** — the lesson's title, its focus tags, and *9 exercises · ~20 min
  · 3 reviews woven in · for today*; **start** on the bar. A lesson left
  half-way says *started*, and the bar says **continue**.
- **building** — the tutor is grading the last lesson's writing and
  authoring the next; nothing to start yet.
- **stale** — built for a day more than three days gone; **build fresh** and
  **play anyway** on the bar.
- **empty** — no lesson at all; **build**, the one wait in fluent.

Under it, three tiles: what is due today (cards, with the new ones and the
grammar items counted), the accuracy of the last five lessons with the
trend in a word, and the words learned with this week's arrivals. The rest
of the bar goes to the course: **review n** (`r`), **cards** (`c`),
**grammar** (`g`), **progress** (`p`), **history** (`h`).

This round *build* is a toast that says what the tutor would do.

### The lesson

A `lesson` panel plays one exercise at a time. The caption says the section
and the kind (`REVIEW · CLOZE`), the count says *3 OF 9*, and a hairline
under them fills as the lesson goes. The prompt is the hero, in the prose
face; a set piece pins its passage in a box above the question, and a
listening withholds the prompt behind *Was hast du gehört?* until the
answer, with **play** (`y`) on the bar and `space` on the keys.

- A **choice** is a row with the digit in a box at its left. The digit
  answers it and checks it at once; the arrows walk the rows and `enter`
  takes the walked one; a press does the same.
- A **gap** or a **translation** is a field, the caret already in it;
  `enter` checks, **check** (`c`) on the bar does the same for a pointer.
- A **free write** is an editor; `enter` checks and `shift+enter` breaks a
  line, as the agent's composer has it.
- **hint** (`h`) unfolds the next hint under the prompt, one at a time, and
  the count of hints shown goes into the record.

A closed answer is graded on the spot. The verdict sits in a wash under
the answer: **Richtig!**, **Fast!** for the same letters with a case or an
umlaut slip, **Nicht ganz.** with *you: der Gebühr* over *→ die Gebühr* and
the explanation. In a choice the right row is marked *answer* and the picked
wrong one *yours*. **next** (`n`) or `enter` goes on. A self-check answer
that is the model answer, or one of the accepted ones, is simply right and
needs no judgement; any other shows the model answer in a box beside what
was written, and the bar is the grade pad: **0 blank · 1 wrong · 2 almost ·
3 hard · 4 good · 5 easy**, the digit in each label the plain key that
fires it. **ask** (`a`) on the bar in every feedback state, and **end**
(`e`) while answering, which closes the lesson where it stands.

Every answer is written the moment it is given — the answer, the result,
the grades, the hints shown, the seconds — and every closed grade and every
self-grade files one review per item the exercise names and moves each
item's SM-2 state. So closing the panel loses nothing, reopening it resumes
at the first unanswered exercise, and one `cmd+z` takes an answer back with
its reviews and its schedule moves. The tutor's later grade is a second
write on the same row, through `fluent.grade`.

After the last exercise the same panel is the **summary**: *Geschafft!*,
*8/9 first try · 89% · 21 min*, the streak line, the calibration of self
against tutor where both exist, the tutor's notes, and one row per
correction — a closed slip with its explanation, or a free answer with the
tutor's grade beside the learner's and the corrected text. Finishing stamps
the lesson's accuracy and minutes on its row and puts a **building**
placeholder on the shelf for tomorrow, which is what the tutor's `fluent.
author` replaces. **ask** and **history** are the bar.

### The tutor

There is no tutor panel of this app's own. **ask** on a lesson's bar is the
workspace's `cmd+shift+a`: the agent app opens a chat joined to the lesson,
carrying it as a chip — the panel's own paragraph and the queries that drew
it, the prompt, the passage, the answer, the model answer and the tutor's
note in full. The original's tutor sidebar with its persistent `claude
--resume` conversation is this chat; the original's background grader and
end-of-session compiler are runs of the same agent with the tools below.

### The review

A `review` panel is the flashcards due today, the queue taken when the
panel opens so a grade does not move it under the learner. The caption says
*CARD 4 OF 18* and *3 NEW · 15 REVIEW*. The card is a box with the word
alone; **show** (`s`), `enter`, `space` or a press turns it: the meaning,
the example in italics, the note. Then the grade pad on the bar, the same
six as the lesson's, and **play** (`y`) beside it. A grade files a review
and moves the card by SM-2, one undo apiece; the next card is up at once.
After the last, *Alles erledigt!*, the count, the right ones and the
minutes, and where the grades went. Nothing due says so and names the
weekday the next card comes.

On a phone the box is display and everything that acts is on the bar at the
foot, which is where a thumb is.

### The lists

`cards`, `grammar` and `lessons` are [rich tables](../book/src/richtable.md).
The deck's row is the word over its meaning, with when it comes due and
its mastery at the right; `@new`, `@weak`, `@learned` and `@mastery` narrow
it. The grammar's rows are grouped under their category (Fälle,
Präpositionen, …) with the level and the mastery stamp at the right;
`@level` and `@category` narrow it. The history's rows are the title and
the day, the accuracy, the minutes and the focus under it; `@date` and
`@accuracy` narrow it. Every cursor previews — a card, a topic, a lesson's
summary — by the shell's rule.

### The cards

`card` is one flashcard whole: the word, the meaning, the example, the
note, then *ease 2.36 · 3 reviews · due tomorrow · mastery 2/5* with the
mastery as five cells, and every grade it was given with the day and where
it came from. **play** (`y`).

`topic` is one rule: the title, *A1 · Fälle · mastery 3/5*, the lessons it
was introduced and last practiced in, then its sections in order — a
paragraph, a table laid out in the mono face (every column as wide as its
widest cell, which is what a monospaced face is for), examples with their
notes, a tip in a box — then *FROM YOUR LESSONS*, the learner's own
stumbles on this rule with the lesson each came from, and *RELATED*, dotted
links that replace the panel with the topic they name.

### Progress

`progress` is read off the rows and edits nothing: the streak and whether
today keeps it; the last 56 days as eight rows of seven cells, each filled
by the minutes studied against the daily goal; mastery per skill as five
cells with the accuracy and the lesson count; the last ten lessons'
accuracy as bars; the three error patterns seen most often with their
wrong → right example and last sighting; the words learned and due.
**history** (`h`).

### The bars, together

Every button acts on what the panel shows and every link goes somewhere
from it, as the [grammar](../book/src/interaction-grammar.md#the-bar)
asks. No bar wears `w`, `z`, `u`, `t`, `i` or `l`, and no bar wears a letter
twice; the grade pad's six wear none, because a digit is the panel's own
key and the label says which. The app's tests say so for every panel over
the demo course, in every phase of the player.

## The store

Every table is prefixed `fluent_`. Dates are days: unix seconds at 00:00
UTC, so *due today* is one comparison and `sqlite3` reads them with
`datetime(due, 'unixepoch')`. The ladder is one step this round.

| Table | What a row is |
|---|---|
| `fluent_learner` | the one learner: name, native and target language, level and goal, daily minutes, streak, when it was last fed |
| `fluent_item` | anything on a schedule, by slug: kind (vocab, grammar, error), one line of content, the SM-2 state (ease, interval, reps, due), when it was last reviewed, a mastery stamp |
| `fluent_card` | the flashcard behind a vocab item: front, back, example, audio, notes |
| `fluent_review` | one grade: item, instant, quality, device, and the lesson and exercise it came from where it did |
| `fluent_lesson` | one sitting: title, the day it is for, focus tags, status (building, ready, done), when it was generated, started and ended, and once done its accuracy, minutes and the tutor's notes |
| `fluent_exercise` | one exercise of a lesson in seq order: section, kind, grading, prompt, passage, audio, choices, accepted, model, hints, explanation, items, difficulty — then what happened: answer, result, self grade, tutor grade, note and fix, hints shown, elapsed, when |
| `fluent_topic` | one rule: title, category, level, summary, mastery stamp, the items it links to, the lessons it was introduced and last practiced in, sections as JSON, related ids |
| `fluent_topic_note` | one of the learner's stumbles on a topic, with the lesson |
| `fluent_mistake` | an error pattern: category, frequency, last seen, a wrong → right example, notes |
| `fluent_skill` | mastery and accuracy per skill |

The six notebooks map onto these: `learner-profile` is the learner row and
the skills; `spaced-repetition` is the items; `vocab-deck` is the cards;
`grammar-kb` is the topics and their notes; `mistakes-db` is the mistakes;
`progress-db` and `session-log` are derived from the lessons and the
reviews and are not stored at all. The review logs the iPhone appended and
the Mac folded in are one table, and [device sync](../book/src/device-sync.md)
carries it: a review is a fact keyed by item, instant and device, so two
devices grading the same deck merge without a cursor file.

The SM-2 step is the original's, bit for bit, and the parity fixture the
original generated from its Python reference is the test.

### The seed

A fresh store in any world but a real one — a scripted run, a library
mount — gets a demo course: a learner twelve days
into a streak, twenty-four cards of which ten are due (three of them new),
grammar and error items on the schedule, six topics with tables and notes,
four error patterns, five skills, twenty-five thin lessons over eight weeks
plus yesterday's whole — eight exercises with two slips and a free answer
the tutor graded — and today's on the shelf, nine exercises across all six
kinds with a set piece and a free write. A real store starts empty, for the
tutor to fill.

## Agents

`App::describe` says the tables above in prose, ending with which tool to
prefer over `sql.write`. Four tools:

| Tool | Writes | What it does |
|---|---|---|
| `fluent.due` | no | every item due today with the card behind it — what a lesson's review section should cover |
| `fluent.lesson` | no | one lesson whole with every answer, grade and note; the newest finished one when unnamed |
| `fluent.grade` | yes | the tutor's 0–5 on a self-check answer, one line of feedback, the corrected text; one undo |
| `fluent.author` | yes | a whole lesson onto the shelf — title, day, focus, the exercises in order — checked for what each kind needs; replaces the building placeholder; one undo removes it |

None asks first: a grade and a lesson are one `cmd+z` away.

The original's pipeline becomes two agent runs over these. The **grader**
is the chat's own answer to a free write: a `fluent.lesson` read, one
`fluent.grade` per self-check answer. The **compiler** is a run that reads
`fluent.lesson` and `fluent.due`, writes its notes, and calls
`fluent.author` once. Both are the agent app's ordinary runs, undoable and
logged; neither is built this round, and what starts them — the summary's
*building* line, a scheduled run — is the open decision below.

## Phases

1. **This round.** The nine panels over the demo course in the panels
   library; answers, grades and finishes as undoable writes; the four tools
   and the SM-2 parity; the bar tests; one e2e suite.
2. **The tutor.** The compiler and the grader as agent runs: a system
   prompt of the course's own (the original's `LEARNING_SYSTEM.md`,
   shortened), started from the summary and from *build*; the *building*
   line reads the run's row. The learner's first lesson comes from the same
   run over an empty course.
3. **Speech.** A `Speech` capability the kernel owns and the platform
   supplies — AVSpeechSynthesizer on macOS, android's TextToSpeech —
   behind **play**; the fake speaks into a toast, as this round does.
4. **The phone.** The review and the lesson at 4×3: the field over the soft
   keyboard, the grade pad wrapped on the bar, `--grid 4x3` scenes in the
   library.
5. **Migration.** A one-shot import of the original's notebooks — the
   items, the cards, the topics, the mistakes, the review logs — so the
   real course starts where it stood.

## To decide in review

- **The desk's shelf words.** *start* / *continue* / *build* / *build fresh*
  / *play anyway* — five words for one box; whether stale needs its own
  pair or a lesson simply plays.
- **Where grades go on a self-check.** This round the learner's grade moves
  the schedule at once and the tutor's later grade only lands on the
  exercise. The original has the tutor's grade authoritative for SM-2. A
  second review row from the tutor, or a rewrite of the first, is the
  question.
- **Sections in the grammar list** are alphabetical by category slug; the
  original's fixed order (Fälle, Präpositionen, Adjektive, Verben, Satzbau,
  Pronomen, Nomen, Sonstiges) wants a rank column or a computed order.
- **The chip's size.** A lesson's chip carries every exercise's prompt,
  passage, answer and model in full; a long set piece may run past the
  32 KiB cap. Whether the chip should carry the current exercise alone.
- **Select-to-translate** (the original's word lookup card): a selection in
  a prompt asking the tutor for the dictionary form. The shell's selectable
  runs and the agent's tools are there; a verb over the selection is not.
- **Replicating the items.** Only reviews replicate this round; the SM-2
  state on `fluent_item` is this device's own. Either the state is derived
  from the reviews on every device, or the item's columns replicate under
  last-writer-wins and two devices grading one card in one day disagree.
- **A lesson's chip on the phone**: `cmd+shift+a` is a chord; the header's
  long-press menu has *start agent with panel context*, and **ask** on the
  bar is there for the same reason.

## Not done, on purpose

- **Confetti, the streak flame, the emoji whitelist.** A finished lesson
  says *Geschafft!* in the prose face and nothing moves.
- **The pause overlay.** Closing the panel is the pause; the lesson resumes
  where it was.
- **The key-hint bar.** The digit is in the choice's box and the letters
  are bold on the bar; there is no legend.
- **A tutor sidebar.** The agent's chat is the tutor, joined like any panel.
- **Colour for right and wrong.** A word says it; the shell's red is for
  errors of the machine, not of the learner.
