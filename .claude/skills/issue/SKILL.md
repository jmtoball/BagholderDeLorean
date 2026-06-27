---
name: issue
description: Draft and create lean, actionable GitHub issues for this repo. Use whenever creating issues, tickets, or work items, or converting a plan into issues. Enforces minimal, final, decision-free tickets and splits research into separate discovery tickets.
---

# Writing issues

One issue = one unit of work a developer can pick up and finish **without asking a question or making a product/architecture decision.**

## Rules

- **Assume the repo.** The reader has CLAUDE.md and the code. Never restate architecture, what a crate does, or why the project exists. Reference things by path (`crates/core/src/lib.rs`, `run_backtest`) — don't describe them.
- **Only what's needed to do the work:** the change, where it lands, how we know it's done, what blocks it. Nothing else. No Background / Motivation / Overview sections.
- **Final and actionable.** If a ticket still holds an open question, an unweighed choice, or "investigate whether…", it is not an implementation ticket — see Discovery.
- **Verifiable acceptance.** Each acceptance item is something you can run or observe. Name the check that proves it (a `cargo test` name, an `e2e/validate.mjs` assertion) — matching the repo's "leave one runnable check" convention.
- **Decide, don't enumerate.** An issue states the chosen approach. Alternatives belong in the discovery ticket that preceded it, not here.
- **Dependencies are relationships, not text.** Express blockers via GitHub's native issue dependencies (blocked-by / blocking) and sub-issues — never as a body section or a list of `#numbers`.
- If it doesn't fit on roughly one screen, it's probably two issues.

## Implementation issue

Title: imperative + specific — `Add square-root slippage to the fill model`.

```
## Task
<1–3 sentences: exactly what to build or change.>

## Where
<files / modules / functions touched, by path.>

## Acceptance
- [ ] <verifiable outcome>
- [ ] <the test/check that proves it, by name>

## Constraints
<only non-obvious invariants or ponytail ceilings.>
```

Drop any section that doesn't apply. Most issues need only Task / Where / Acceptance. Blockers go in the issue's GitHub relationships, not the body.

## Discovery issue

If a task can't be made actionable without research, a decision, or a spike, do **not** write a hand-wavy implementation ticket. Write a discovery ticket whose deliverable *is the answer*, and which spawns the implementation ticket(s) when it closes.

Title: `Discovery: <the question>`.

```
## Question
<the exact unknown or decision to resolve.>

## Deliverable
<the concrete output — a chosen approach / data-source verdict / spike result — and where it's recorded.>

## Done when
- [ ] <decision made and recorded>
- [ ] Implementation issue(s) opened
```

The spawned implementation issue(s) link back via a GitHub blocked-by relationship — what this discovery gates lives in that relationship, not in the body.

## Reject

- Restating architecture or pasting from research docs.
- "Background" / "Motivation" / "Context" prose the repo already covers.
- Open choices left in an implementation ticket → make it Discovery, or decide.
- Acceptance criteria you can't run or observe.
- "While we're here" extras → separate issue.

## Creating

`gh issue create` with the epic label + milestone; body = the template above. Set blockers as GitHub issue dependencies / sub-issues (the issue's Relationships), not in the body.
