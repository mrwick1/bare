# 2026-11-25: the Navigator interface object

Branch `navigator-interface`.

## Change

- `browser_web.rs`: the global `Navigator`, an interface object with an illegal constructor. Its `prototype` is the prototype
  of `navigator`. It is lazy, like the other globals of the file: a realm pays for it only at the first read.
- Cause: WhatsApp link attempt 5. A page module reads `"setAppBadge" in Navigator.prototype`, and bare threw
  `ReferenceError: Navigator is not defined`.
- Limit: a worker gets `Navigator` too. The standard gives a worker `WorkerNavigator` instead.

## Tests

- New `crates/bare/src/browser_web_navigator_tests.rs`: the type, the prototype link, `instanceof`, the illegal constructor and
  the enumerable flag. It failed before the fix (`ReferenceError`).
- `t`: all pass.

## Cost

archMachine. Base = main with #1087, new = this branch. Minimum of 3 runs (`instr.sh`).

- `cost_matrix.sh -g 'web_apis_load|browser_load|session_load'`: 24 rows, none over +0.5%. Mean glibc +0.024%.
  `web_apis_load segmenter 1` and `10` fail to run, on main too.

## Not tested

- The WhatsApp link with this fix. The throw is in a telemetry function (`WAWebPlatformEstimate`), so it does not explain the
  failed link. Windows.
