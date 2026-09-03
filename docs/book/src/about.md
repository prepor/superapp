# About this Book

This book describes how Superapp works and why it works that way. Superapp is
my personal project, and I am its only user. It is not designed for many
people. If the book and the code disagree, one of them must be fixed.

## Goals

- Describe the current product, not its history or abandoned designs.
- Explain user-visible behavior together with the relevant implementation.
- Prefer short explanations and plain language.
- Name code when it helps readers verify a claim. Avoid large snippets that
  are likely to become stale.
- Keep unresolved decisions in [Open Questions](./open-questions.md).

## Workflow

Start user-visible work with a change request in
`docs/planning/cr-<nnn>-<name>.md`. Write it as the book should read after the
change is complete.

As the code lands, move the lasting information into this book. Delete the
change request when the work is complete. Git keeps the history; the book only
needs the current design.
