"""Replace personal data in recorded protocol lines: email addresses and the home folder name.

Run as a script to rewrite recordings in place: python anonymize.py <file>...
"""

import re
import sys
from pathlib import Path

EMAIL = re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)+")
HOME_NAME = Path.home().name


def scrub(text):
    return EMAIL.sub("user@example.com", text).replace(HOME_NAME, "user")


def main():
    for name in sys.argv[1:]:
        path = Path(name)
        path.write_bytes(scrub(path.read_bytes().decode("utf-8")).encode("utf-8"))


if __name__ == "__main__":
    main()
