#!/usr/bin/env python3
"""Write the standard suite of 40 tests (docs/design-suite.md, section 5.2) into DIR.

usage: gen_suite.py DIR

setup.steps is the login that the shared tests start from. A test that begins with `open ... state="${STATE}"` is shared: the runner
logs in once (setup.steps) and each such test starts from the saved session. A test with `sign out`, `password` or `delete` in its
steps, or with a line `# own`, gets its own login. A test with no `state=` line is its own already.
  26 read    5 pages: click, the heading, the list count after the fetch       (shared)
   2 iso     a pair: the first leaves a cookie and storage, the second must not see them (shared)
   4 text    1 click, then only text, title and count checks                   (shared)
   4 write   add a note through the page, check the list                       (shared)
   2 signout sign out, check the login page                                    (own by the lint)
   1 wrong   a wrong password                                                  (own: no state line)
   1 marked  a read test that the author marks `# own`                         (own by hand)
"""
import os
import sys

out = sys.argv[1]
os.makedirs(out, exist_ok=True)

START = 'open ${BASE}/home state="${STATE}"\nexpect css="h1" text="Heading home"\n'


def write(name, text):
    with open(os.path.join(out, name + ".steps"), "w") as f:
        f.write(text)


def visits(i, n=5):
    s = ""
    for k in range(n):
        p = (i + k) % 8 + 1
        s += f'click role=link name="Page {p}"\nexpect css="h1" text="Heading p{p}"\nexpect css="#count" text="20"\n'
    return s


write(
    "setup",
    """# The login that the shared tests start from. The runner runs it once and saves the session to ${STATE}.
open ${BASE}/login
type css="input[name=email]" "${EMAIL}"
type css="input[name=password]" "${PASSWORD}"
click role=button name="Sign in"
expect url~"/home"
expect css="h1" text="Heading home"
save-state "${STATE}"
""",
)

n = 0
for i in range(26):
    n += 1
    write(f"{n:02d}-read-{i:02d}", "# read: 5 pages\n" + START + visits(i))
# The pair that checks the isolation of the shared login: the first test leaves a cookie and storage behind.
n += 1
write(
    f"{n:02d}-iso-a",
    "# iso: leave a cookie and storage behind\n" + START
    + "expect js \"(localStorage.setItem('leak','1'), sessionStorage.setItem('leak','1'), true)\"\n"
    + "cookie set leak 1\nexpect cookie leak value=\"1\"\n" + visits(1, 2),
)
n += 1
write(
    f"{n:02d}-iso-b",
    "# iso: another test must not see them\n" + START
    + "expect js \"localStorage.getItem('leak') === null && sessionStorage.getItem('leak') === null\"\n"
    + "expect cookie leak absent\nexpect cookie sid\n" + visits(2, 2),
)
for i in range(4):
    n += 1
    p = i % 8 + 1
    write(
        f"{n:02d}-text-{i}",
        "# text: only text, title and count checks after one click\n" + START
        + f'click role=link name="Page {p}"\nexpect css="h1" text="Heading p{p}"\nexpect title="p{p}"\nexpect css="#count" text="20"\n'
        + 'expect css="tr" count 60\nexpect css="nav a" count 10\nexpect css="p" text~"Items"\n',
    )
for i in range(4):
    n += 1
    note = f"note-{i}"
    write(
        f"{n:02d}-write-{i}",
        "# write: add a note through the page\n" + START
        + f'click role=link name="Notes"\nexpect css="h1" text="Heading notes"\ntype css="#note" "{note}"\nclick role=button name="Add note"\nexpect css="#notes" text~"{note}"\n',
    )
for i in range(2):
    n += 1
    write(
        f"{n:02d}-signout-{i}",
        "# signout: the lint marks it own (sign out)\n" + START
        + 'click role=button name="Sign out"\nexpect url~"/login"\nexpect cookie sid absent\nopen ${BASE}/p1\nexpect url~"/login"\n',
    )
n += 1
write(
    f"{n:02d}-wrong-0",
    '# wrong: a wrong password. It has no state line, so it is its own.\nopen ${BASE}/login\ntype css="input[name=email]" "${EMAIL}"\ntype css="input[name=password]" "not the password"\n'
    + 'click role=button name="Sign in"\nexpect css="p" text="Sign-in failed"\nexpect cookie sid absent\n',
)
n += 1
write(f"{n:02d}-marked-0", "# own\n# marked by hand: this read test wants its own login\n" + START + visits(5, 3))
print(f"{n} tests and setup.steps in {out}")
