"""Write a sample its4tasks board for visual testing: seed_board.py DATA_HOME

Creates DATA_HOME/its4tasks/board.json with ~12 days of history relative to now:
open blockers, resolved blockers (one with an Unblock message), multi-line
comments (a title line plus details), starred comments, next steps (open,
done, and one on a blocked task, where red wins), a done task closed
with a message, tasks of different freshness, and subtasks: a parent blocked
by its subtasks (focused, with one of them) and one that isn't. Nothing
loads it on its own: import it into a test user's board as AGENTS.md shows
(examples/import_change.rs in its4tasks-core turns it into an Import change),
or with "Import board.json…" in the app.
"""
import datetime as dt
import json
import pathlib
import sys

now = dt.datetime.now(dt.timezone.utc).replace(microsecond=0)


def ago(**k):
    return (now - dt.timedelta(**k)).isoformat().replace("+00:00", "Z")


def note(text, at, blocked_at=None, resolved=None, starred=False, action=None, step=None, step_done=None):
    """`step` is when it became a next step; `step_done` when it was ticked off."""
    d = {"at": at, "text": text}
    if step:
        d["next_step"] = {"Done": {"since": step, "at": step_done}} if step_done else {"Open": {"since": step}}
        starred = True  # The app stars next steps, like blockers.
    if action:
        d["action"] = action
    if blocked_at:
        d["blocked_at"] = blocked_at
        d["blocker"] = {"Resolved": {"at": resolved}} if resolved else "Open"
    if starred:
        d["starred"] = True
    return d


BUDGET = "Need budget approval from finance\nThe venue deposit is over my limit, so finance has to sign off first."

tasks = [
    dict(title="Validate solver on client data", created_at=ago(days=9), updated_at=ago(days=3), notes=[
        note("Ran first batch, results look off\nObjective is ~15% above their baseline. Checking whether they use the same"
             " constraints.", ago(days=4)),
        note("The input case that they sent me doesn't work, I have asked for a new one and waiting for new one.",
             ago(days=3, hours=2), ago(days=3, hours=2), starred=True),
        note("Pinged them again about the input case", ago(days=1)),
        note("Rerun with their constraints once the new case arrives", ago(hours=20), step=ago(hours=20)),
    ]),
    dict(title="Fix login bug", created_at=ago(days=6), updated_at=ago(minutes=20), notes=[
        note("Waiting on staging access", ago(days=2, hours=3), ago(days=2, hours=3), ago(minutes=20), starred=True),
        note("Repro'd on staging", ago(minutes=10)),
    ]),
    dict(title="Plan offsite", created_at=ago(days=8), updated_at=ago(days=5), notes=[
        note("Shortlist venues", ago(days=8)),
        note(BUDGET, ago(days=7), ago(days=6, hours=20), ago(days=5, hours=1), starred=True),
        note("Finance signed off on the full amount", ago(days=5, hours=1), starred=True,
             action={"Unblocked": {"quote": BUDGET, "blocked_since": ago(days=6, hours=20)}}),
        note("Booked the venue", ago(days=5), starred=True),
        note("Ask for dietary requirements", ago(days=4), step=ago(days=4), step_done=ago(days=1)),
        note("Send the agenda\nDraft is in the shared folder; needs the room plan and the times for the talks.",
             ago(days=1), step=ago(days=1)),
        note("Book the bus", ago(hours=3), step=ago(hours=3)),
    ]),
    # Blocked since before "Plan offsite"'s blocker was resolved, so that pair can't move here.
    dict(title="Renew passport", created_at=ago(days=12), updated_at=ago(days=6), notes=[
        note("Waiting for a photo appointment", ago(days=6), ago(days=6), starred=True),
    ]),
    dict(title="Buy groceries", created_at=ago(days=10), updated_at=ago(days=2), done=True, done_at=ago(days=2),
         notes=[note("Milk, eggs, coffee", ago(days=10)),
                note("All bought, coffee was on sale", ago(days=2), action="Closed")]),
    dict(title="Read paper on CRDTs", created_at=ago(hours=5), updated_at=ago(hours=2),
         notes=[note("Section 3 is the interesting part", ago(hours=2))]),
]
# Subtasks: a parent blocked because all its open subtasks are (one of them
# has a done subtask of its own, which doesn't count), and an unblocked parent.
tasks += [
    # Folded, but its subtask "Update docs" stays listed: it's on the board.
    dict(title="Ship v2 release", created_at=ago(days=6), updated_at=ago(days=6), folded=True, notes=[]),  # 6
    dict(title="Update docs", parent=6, created_at=ago(days=6), updated_at=ago(days=1), notes=[
        note("Waiting on screenshots from design", ago(days=1), ago(days=1), starred=True),
    ]),  # 7
    dict(title="Security review", parent=6, created_at=ago(days=5), updated_at=ago(days=3), notes=[
        note("Security team is booked until next week", ago(days=3), ago(days=3), starred=True),
    ]),  # 8
    dict(title="Scan dependencies", parent=8, created_at=ago(days=5), updated_at=ago(days=4), done=True,
         done_at=ago(days=4), notes=[note("No findings", ago(days=4), action="Closed")]),  # 9
    dict(title="Bump version", parent=6, created_at=ago(days=6), updated_at=ago(hours=6), done=True,
         done_at=ago(hours=6), notes=[]),  # 10
    dict(title="Book flights", parent=2, created_at=ago(days=5), updated_at=ago(days=4),
         notes=[note("Compare train vs plane", ago(days=4))]),  # 11
]
for i, t in enumerate(tasks):
    t["id"] = i
    t.setdefault("done", False)

path = pathlib.Path(sys.argv[1]) / "its4tasks" / "board.json"
path.parent.mkdir(parents=True, exist_ok=True)
path.write_text(json.dumps({"tasks": tasks, "slots": [0, 6, 2, 7], "next_id": len(tasks)}, indent=1))
print(path)
