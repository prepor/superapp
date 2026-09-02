# About this Book

This book is the single source of truth for Superapp. It describes both **what**
the system does and **why** (the product), and **how** it is implemented. It is
the same workflow as the Stelaxis book: if the book and the code disagree, that
is a bug.

## Goals

- **One source of truth.** Everything important about Superapp lives here.
- **Current state only.** The book describes how Superapp works *today*. Not
  history, not aspirations, not deprecated approaches. The one exception is
  [Open Questions](./open-questions.md), which records design decisions that
  are genuinely still open — that they are open *is* the current state.
- **Product and implementation, side by side.** Each chapter mixes behavior
  and the reasoning behind it with the structure that realizes it.
- **Concise and conceptual.** Chapters name modules and files where useful but
  avoid large code snippets that would drift.
- **Readable by humans and agents.** Both use this book as primary reference.

## Workflow

Changes to Superapp — new panel kinds, behavior changes, anything user-visible —
are proposed as **Change Requests to the book**: a markdown document under
`docs/planning/cr-<nnn>-<name>.md` describing how the book should read once the
change is done, pushed at the *start* of the work and shrinking as it lands.

A CR is scaffolding, not a record. When the work lands, everything of it that
still matters is *in the book* — so the CR is **deleted** in the same change,
and `docs/planning/` holds only what is in flight. Git keeps the reasoning for
anyone who wants it; the book keeps the decision, and does not name the CR
that carried it. A chapter that says *when* something changed, or what it
replaced, has stopped describing the current state.
