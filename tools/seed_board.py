"""Write a sample focal board for visual testing: seed_board.py DATA_HOME

Creates DATA_HOME/focal/board.json with ~12 days of history relative to now:
an open blocker, resolved blockers (one with an Unblock message), starred
comments, a done task closed with a message, tasks of
different freshness. Run the app with XDG_DATA_HOME=DATA_HOME to load it.
"""
import datetime as dt
import json
import pathlib
import sys

now = dt.datetime.now(dt.timezone.utc).replace(microsecond=0)


def ago(**k):
    return (now - dt.timedelta(**k)).isoformat().replace("+00:00", "Z")


def note(text, at, blocked_at=None, resolved=None, starred=False, action=None):
    d = {"at": at, "text": text}
    if action:
        d["action"] = action
    if blocked_at:
        d["blocked_at"] = blocked_at
        d["blocker"] = {"Resolved": {"at": resolved}} if resolved else "Open"
    if starred:
        d["starred"] = True
    return d


tasks = [
    dict(title="Validate solver on client data", created_at=ago(days=9), updated_at=ago(days=3), notes=[
        note("Ran first batch, results look off", ago(days=4)),
        note("The input case that they sent me doesn't work, I have asked for a new one and waiting for new one.",
             ago(days=3, hours=2), ago(days=3, hours=2), starred=True),
        note("Pinged them again about the input case", ago(days=1)),
    ]),
    dict(title="Fix login bug", created_at=ago(days=6), updated_at=ago(minutes=20), notes=[
        note("Waiting on staging access", ago(days=2, hours=3), ago(days=2, hours=3), ago(minutes=20), starred=True),
        note("Repro'd on staging", ago(minutes=10)),
    ]),
    dict(title="Plan offsite", created_at=ago(days=8), updated_at=ago(days=5), notes=[
        note("Shortlist venues", ago(days=8)),
        note("Need budget approval from finance", ago(days=7), ago(days=6, hours=20), ago(days=5, hours=1), starred=True),
        note("Finance signed off on the full amount", ago(days=5, hours=1), starred=True,
             action={"Unblocked": {"quote": "Need budget approval from finance", "blocked_since": ago(days=6, hours=20)}}),
        note("Booked the venue", ago(days=5), starred=True),
    ]),
    dict(title="Renew passport", created_at=ago(days=12), updated_at=ago(days=12), notes=[]),
    dict(title="Buy groceries", created_at=ago(days=10), updated_at=ago(days=2), done=True, done_at=ago(days=2),
         notes=[note("Milk, eggs, coffee", ago(days=10)),
                note("All bought, coffee was on sale", ago(days=2), action="Closed")]),
    dict(title="Read paper on CRDTs", created_at=ago(hours=5), updated_at=ago(hours=2),
         notes=[note("Section 3 is the interesting part", ago(hours=2))]),
]
for i, t in enumerate(tasks):
    t["id"] = i
    t.setdefault("done", False)

path = pathlib.Path(sys.argv[1]) / "focal" / "board.json"
path.parent.mkdir(parents=True, exist_ok=True)
path.write_text(json.dumps({"tasks": tasks, "slots": [0, 1, 2, 3], "next_id": len(tasks)}, indent=1))
print(path)
