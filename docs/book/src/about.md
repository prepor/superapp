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

## The two parts

The book follows the code's own split. A chapter in *The shell* states a rule
or describes a component, and may use an app as a one-line example. A chapter
in *Apps* states what that app does and links to the rule it follows rather
than restating it. [Architecture](./architecture.md) describes the layers and
[Apps](./apps.md) is the contract between them.

## Workflow

Update the relevant chapters alongside the code. Check behavior, ownership,
configuration and limits against the implementation and its tests before
describing a feature as available.

Keep unresolved choices in [Open Questions](./open-questions.md) and known
implementation gaps in the chapter they affect. Prototypes may illustrate a
design, but their instructions should link here for current behavior. Git
keeps the history; the book is the maintained reference.
