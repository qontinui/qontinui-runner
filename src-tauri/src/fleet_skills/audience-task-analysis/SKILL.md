---
name: audience-task-analysis
description: "Derive what a page, screen or panel should contain from what each audience role comes to it to do, before any layout is chosen. Seven written steps: one arrival per role; what they answer for and how far they are from it; their ranked questions; what could answer each; the rent test, element by element; layout by question, per role and situation; the walk as each role. Leaves a per-surface section in the audience profile and a question-to-element map in the plan. Use when a plan or a spec creates a surface or changes what one shows or its order."
user-invocable: true
---

# Audience task analysis — the method

Do this before you decide anything about layout. Write each step down; the written steps are part of the design.

## Step 1 — One arrival per role

For each role that reaches the surface, write one short scene: who they are, where they are standing, what
device is in their hand, what just happened that made them open this page, and what they will do in the next two
minutes. Use the profile's own words for their job. If two roles would write the same scene, they are one role
here; if one role has two clearly different scenes (start of day, in the middle of a job), write both.

## Step 2 — What they answer for, and how far they are from it

Before any question, write one sentence per role: the OUTCOME this person is held to — not the things they click.
A job description lists tasks ("enter", "check", "approve"); the outcome is what those tasks are for ("every
guest's room is ready before they arrive", "no claim is settled late or twice", "every shift is staffed").

Then write what stands between now and that outcome: what is still missing, still open, late, blocked or
unconfirmed. That GAP is what a person opens a working page to see. It is a difference — what is needed against
what is there, what was planned against what happened, what is due against what is done — and it is not the same
thing as a list of records. Forty bookings are records; "these six rooms are not ready for today's arrivals" is
the gap.

Name the gap for each role in their words, whether or not any screen or interface shows it today. If you cannot
say what "done" means for a role, you do not yet know what their page is for.

Name also the unit the person works in — one site, one case, one shipment, one patient. The gap is stated per
that unit, and later the page is grouped by it. A person who thinks "what about my site" is not helped by one
list of everything in date order.

## Step 3 — Their questions, ranked

For each scene, list the questions the person wants answered and the things they want to start, phrased as they
would say them ("Which of today's rooms are not ready yet?"), not as the system names its data ("bookings"). The
first question is nearly always the gap of Step 2; the next is usually "is what I already set in motion on its
way"; then come the things they start from here. Rank them:

- **every visit** — they ask it each time they open the page;
- **when something changed** — they need it only when there is news or an exception;
- **rarely** — a few times a year, or once.

A question is about the state of the person's world. "What is the name of my own company?" is not a question
anybody opens a page to ask.

## Step 4 — What could answer each question

Only now look at what the system can provide. For each ranked question, name the facts that would answer it and
where they come from. The gap is rarely held anywhere as such: it has to be worked out by setting two things the
system keeps apart against each other — what was promised against what was delivered, what is required against
what is available, what is owed against what is paid. Design that comparison, say which sources it needs and who
should compute it, and show its RESULT — the difference — not the two lists side by side for the person to
subtract in their head.

If nothing can answer a top question today, do not stop at saying so. A top question with no answer is the
most important thing this analysis finds. Work out the smallest thing that would make it answerable: usually a
definition (what the product will treat as the plan, the need, the due date) that lets two things it already
keeps be set against each other; sometimes one field or one read. Choose it, write it down as an assumption to
be confirmed, and design the page with it. Do not swap the question for an easier one the data happens to
answer, and do not quietly drop it.

## Step 5 — The rent test, element by element

List every element you are tempted to place — including the ones that feel obligatory. For each:

| Element | Whose question does it answer | How often needed here | Changes between visits? | Verdict |
| --- | --- | --- | --- | --- |

Verdicts: **first screen** (answers an every-visit question), **on the page, lower or collapsed** (answers a
when-something-changed question — show it only when there is something to show), **one step away** (rarely
needed, or the same on every visit), **not here** (answers nobody's question on this surface).

Things that usually fail the test, and must be argued in, not out: the organisation's own master data; a welcome
or explanation of what the page is; a block per module "so the page is complete"; links that repeat the
navigation; a count that never changes; a notice that says the same thing on every visit; a list of the newest
records where the person wanted to know what is still open.

## Step 6 — Lay it out by question, per role, per situation

Order the page by the ranked questions of the role looking at it. State, for the smallest and the largest
supported screen, exactly what is visible without scrolling — and check it against the every-visit questions.
Where one role's first screen would be another's noise, give them different pages, not one page with gaps.

Make every row carry more: one line per record where a table or a compact row can do it, the fields that differ
between rows first, labels shortened where the full word is one step away. Use a disclosure, a tab or a menu for
what is needed only sometimes, and a search or filter once a list is longer than a screen.

## Step 7 — Walk it as each role

Read your draft as each person in their scene. Write down, in their voice, what they see first, whether it tells
them how far they are from done, and what is in their way. Every objection changes the design or gets a written
answer.

## A worked example — the lending desk of a public library

*Roles.* Desk staff, standing, a scanner in one hand, a queue in front of them. Branch librarian, at a desk,
once a morning.

*What they answer for.* Desk staff: every reader leaves with what they came for, and every reserved item reaches
the reader who reserved it. The gap: reservations that are due to be collected and are not on the pick-up shelf
yet. Branch librarian: the branch's stock comes back. The gap: items overdue long enough that someone must act.

*Desk staff, every visit:* "Which reserved items still have to be fetched from the stacks?" — "Whose turn is it
and what do they have?" *When something changed:* "Which reservations have waited on the shelf past their date?"
*Rarely:* opening hours.

*What answers it.* No single list holds "still to fetch": it is the reservations due today set against what has
been scanned onto the pick-up shelf. The page shows the difference — the eleven items not yet fetched, by
shelf mark — not the reservation list and the shelf list next to each other.

*A question no record answers.* "Which readers are waiting for an item that is in the building?" Nothing is
stored as "waiting". The smallest definition that answers it: a reservation that is due and has no pick-up scan
is waiting. Written down as an assumption to confirm, and the page is designed with it.

*Rent test.* Items still to fetch, and the scan field: first screen for desk staff. Reservations past their
date: on the page, shown only when there is one. The branch's address and opening hours: one step away — nobody
at the desk asks the page where they work. A row of tiles, one per module (Catalogue, Members, Loans, Fees): not
here — it answers no question; the navigation already offers them. Yesterday's loan count: not here for desk
staff; for the librarian it becomes "overdue by more than 30 days", which is the gap they actually answer for.

*Result.* Two different pages. The desk page is what is still to fetch, a scan field and one borrower's loans,
usable one-handed. The librarian's page is a short list of exceptions with the action beside each.

## An unanswerable top question is yours to settle

Step 4 says to write the chosen definition down "as an assumption to be confirmed", and the served clause says
"for the operator to confirm". Both mean the operator can correct it later. Confirming it is not a step you wait
for, and not a question you ask. Record the assumption where the next reader will find it: in the surface's spec, and in the
audience profile's per-surface section as a `proposed` question. Then design with it and go on. Asking the
operator whether the definition is right is not part of this method. Two cases only go to the operator, and the
served clause names them: two roles' top questions cannot share a first screen and no intent document says whose
surface it is; or showing the gap needs a capability a declared non-goal excludes.

## What you leave behind

1. **In the audience profile**, a per-surface section for this surface: for each role the situation, what they
   answer for, the unit they work in, what they do next, and their questions, each with a stable id
   (`<surface>.<role>.<n>`), a rank (`every visit`, `when something changed`, `rarely`) and a status. What you
   derived is `proposed`. Only what the operator or a customer said is `declared`. Where the profile already has
   the section, add to it; never renumber it. Where the document's tier asks for a notification, post the
   finding first.
2. **In the plan or the spec**, the section `## Audience and questions`: who arrives, the questions by id, every
   first-screen element with the question it answers, per role and per width, what you decided NOT to put on the
   surface and why, and each question the product cannot answer yet with the definition, field or read you chose.

The rule this method serves is the served `ux-priorities` clause
`a-surface-is-designed-from-what-its-audiences-come-to-do`. Read it; this file does not replace it.
