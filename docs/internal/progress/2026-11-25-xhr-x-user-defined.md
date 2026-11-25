# XMLHttpRequest responseText with charset x-user-defined (2026-11-25)

## Change

`responseText` decodes the charset x-user-defined. A byte below 0x80 is itself. A byte from 0x80 is U+F780 + (byte - 0x80).
Before, the charset was `Unsupported`, and the fault stopped the page.

WhatsApp Web reads binary data with `overrideMimeType("text/plain; charset=x-user-defined")`. The stopped page could not send
a message. With the fix, a message sends and is delivered.

## Tests

- New: `browser_xhr_charset_tests.rs`. It fails before the fix (the fault) and passes after it.
- `t`: 5 groups, all pass.

## Cost

No row in `cost_rows.txt` covers XMLHttpRequest. The change adds one branch to `responseText`. No cost run.

## test262

No change to Vex. No run.
