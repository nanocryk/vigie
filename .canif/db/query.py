#!/usr/bin/env python3
# ---
# description: >
#   Run one read-only SQL query on vigie2's history database (~/.vigie2/history.db):
#   events, sessions, usage samples. Rows tab-separated, long cells cut.
# risky: true
# args:
#   sql:
#     help: One SQL statement; ATTACH, VACUUM and the query_only pragma are refused
#   width:
#     type: int
#     min: 20
#     max: 100000
#     optional: true
#     default: 300
#     help: Characters kept per cell
#   limit:
#     type: int
#     min: 1
#     max: 100000
#     optional: true
#     default: 200
#     help: Rows printed
# ---
import os
import re
import sqlite3
import sys

sys.stdout.reconfigure(encoding="utf-8")
sql = os.environ["CANIF_SQL"]
width = int(os.environ["CANIF_WIDTH"])
limit = int(os.environ["CANIF_LIMIT"])

# VACUUM INTO writes a new file even from a read-only connection.
if re.search(r"\bvacuum\b", sql, re.IGNORECASE):
    sys.exit("refused: VACUUM")

path = os.path.expanduser("~/.vigie2/history.db")
db = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
db.execute("PRAGMA query_only = 1")


def authorize(action, arg1, arg2, _db, _trigger):
    if action == sqlite3.SQLITE_ATTACH:
        return sqlite3.SQLITE_DENY
    if action == sqlite3.SQLITE_PRAGMA and (arg1 or "").lower() == "query_only":
        return sqlite3.SQLITE_DENY
    return sqlite3.SQLITE_OK


db.set_authorizer(authorize)


def cell(value):
    text = "NULL" if value is None else str(value)
    text = text.replace("\n", "\\n").replace("\t", "\\t")
    return text if len(text) <= width else text[:width] + "..."


try:
    cursor = db.execute(sql)
except sqlite3.Error as err:
    sys.exit(f"sql error: {err}")
if cursor.description is not None:
    print("\t".join(column[0] for column in cursor.description))
shown = 0
for row in cursor:
    if shown == limit:
        extra = 1 + sum(1 for _ in cursor)
        print(f"({extra} more rows)")
        break
    print("\t".join(cell(value) for value in row))
    shown += 1
