#!/usr/bin/env python3
"""Create a private-data-free pre-meeting-brief fixture in a NEW directory.

Usage: scripts/pre-meeting-brief-fixture.py /tmp/stenly-brief-probe

Writes, all synthetic and authored:
  calendar.ics       three upcoming events (no EventKit, no Google, no Kody)
  data/context/*.json a brief for "Pilot review" (what the context agent saves
                     N minutes before a meeting); none for the other two
  recordings/        one past "Pilot review" with action items
  config-on.toml     context enabled: "Pilot review" has a brief, "Weekly sync"
                     is shown as being gathered (STENLY_BRIEF_PREPARING)
  config-off.toml    context disabled (the default): no brief, one-time hint

The model is never called: llm.enabled = false keeps the context agent from
running, and the calendar source is "ics" so macOS Calendar access is not used
(EventKit is not isolated by STENLY_CONFIG). Never point this at real data.
"""
import datetime
import json
import pathlib
import sys

if len(sys.argv) != 2:
    sys.exit(__doc__)
root = pathlib.Path(sys.argv[1]).expanduser().resolve()
root.mkdir(parents=True, exist_ok=False)
data = root / "data"
recordings = root / "recordings"
(data / "context").mkdir(parents=True)
recordings.mkdir()

now = datetime.datetime.now(datetime.timezone.utc).replace(second=0, microsecond=0)
ME = "alex@example.test"
fmt = lambda t: t.strftime("%Y%m%dT%H%M%SZ")


def event(uid, title, start, mins, guests, desc):
    return dict(uid=uid, title=title, start=start, end=start + datetime.timedelta(minutes=mins), guests=guests, desc=desc)


events = [
    event("pilot-review@example.test", "Pilot review", now + datetime.timedelta(minutes=40), 45,
          [("Sam Rivera", "sam@guest.example.test"), ("Priya Shah", "priya@guest.example.test")],
          "Review the pilot scope and the open pricing question."),
    event("weekly-sync@example.test", "Weekly sync", now + datetime.timedelta(hours=3), 30,
          [("Jordan Ruiz", "jordan@example.test")], "Weekly team sync."),
    event("budget@example.test", "Budget check-in", now + datetime.timedelta(days=1, hours=1), 30,
          [("Kim Okafor", "kim@example.test")], "Quarterly budget check-in."),
]

lines = ["BEGIN:VCALENDAR", "VERSION:2.0", "PRODID:-//stenly synthetic fixture//EN", f"X-WR-CALNAME:{ME}"]
for e in events:
    lines += ["BEGIN:VEVENT", f"UID:{e['uid']}", f"SUMMARY:{e['title']}", f"DTSTART:{fmt(e['start'])}",
              f"DTEND:{fmt(e['end'])}", f"DESCRIPTION:{e['desc']}", f"ORGANIZER;CN=Alex Example:mailto:{ME}",
              f"ATTENDEE;CN=Alex Example;PARTSTAT=ACCEPTED:mailto:{ME}"]
    lines += [f"ATTENDEE;CN={n};PARTSTAT=ACCEPTED:mailto:{m}" for n, m in e["guests"]]
    lines.append("END:VEVENT")
lines.append("END:VCALENDAR")
(root / "calendar.ics").write_text("\r\n".join(lines) + "\r\n")

# The brief the context agent would have saved for "Pilot review". The file is
# named like stenly_core::context::event_cache_path: "<uid>@<unix start>" with
# every other character turned into "_".
pilot = events[0]
event_id = f"{pilot['uid']}@{int(pilot['start'].timestamp())}"
safe = "".join(c if c.isascii() and (c.isalnum() or c in "-_.") else "_" for c in event_id)[:150]
brief = dict(
    brief=("Third pilot check-in with Example Labs. Sam wants to confirm the scope for the second phase and is waiting on a "
           "pricing proposal; Priya joins for the first time to cover the security review. The main risk is the "
           "unanswered data-retention question from last week."),
    people=[dict(name="Sam Rivera", role="Product Lead", company="Example Labs", email="sam@guest.example.test"),
            dict(name="Priya Shah", role="Security Engineer", company="Example Labs", email="priya@guest.example.test")],
    companies=[dict(name="Example Labs", notes="Prospect running a two-phase pilot")],
    terms=["Example Labs", "data retention"],
    open_threads=["Data-retention answer still owed to Priya", "Pricing proposal for phase two"],
    sources=[dict(title="Pilot review, last week", tool="past_meetings_search"),
             dict(title="Pilot scope (notes)", tool="vault_read")],
    gathered=dict(at=now.isoformat(), model="synthetic", secs=12.0, steps=[], stopped=None, note=None),
    event=dict(uid=pilot["uid"], title=pilot["title"], start=pilot["start"].isoformat()),
)
(data / "context" / f"{safe}.json").write_text(json.dumps(brief, indent=2))

# One past "Pilot review" with action items (what "last meeting" shows).
past_start = now - datetime.timedelta(days=7)
session = recordings / "2026-pilot-review"
session.mkdir()
iso = lambda t: t.isoformat()
past_event = dict(id=f"{pilot['uid']}@{int(past_start.timestamp())}", uid=pilot["uid"], title="Pilot review",
                  start=iso(past_start), end=iso(past_start + datetime.timedelta(minutes=45)), link=None, organizer=None,
                  attendees=[dict(name="Sam Rivera", email="sam@guest.example.test", status=None)],
                  description=None, calendar=ME, declined=False, recurring=False)
(session / "meta.json").write_text(json.dumps(dict(
    title="Pilot review", started=iso(past_start), ended=iso(past_start + datetime.timedelta(minutes=45)),
    event=past_event, state="Done", note_path=None, error=None, routes=["synthetic fixture: no audio recorded"])))
(session / "summary.json").write_text(json.dumps(dict(
    summary="Reviewed phase one results and agreed to scope phase two.",
    action_items=[dict(owner="Alex", task="Send the phase-two pricing proposal"),
                  dict(owner="Sam", task="Confirm the pilot success criteria"),
                  dict(owner="Alex", task="Answer the data-retention question")])))

q = lambda p: json.dumps(str(p))  # JSON basic strings are valid TOML basic strings
base = f"""[app]
onboarded = true
launch_at_login = false
[me]
name = "Alex Example"
emails = ["{ME}"]
auto = false
[output]
audio_dir = {q(recordings)}
notes_dir = {q(root / 'notes')}
[llm]
enabled = false
[calendar]
source = "ics"
ics_urls = [{q(root / 'calendar.ics')}]
kody_url = ""
auto_record_titles = []
[detect]
ad_hoc_calls = false
[dictation]
enabled = false
sounds = false
[mcp]
http = false
"""
(root / "config-on.toml").write_text(base + "[context]\nenabled = true\n")
(root / "config-off.toml").write_text(base + "[context]\nenabled = false\n")

print(json.dumps(dict(root=str(root), data=str(data), config_on=str(root / "config-on.toml"),
                      config_off=str(root / "config-off.toml"), calendar=str(root / "calendar.ics")), indent=2))
