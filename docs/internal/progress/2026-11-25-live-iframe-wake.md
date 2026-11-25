# A live page wakes for its child pages

## Problem

- Outlook took 14.5 s to show the Microsoft login form in bare. Chrome takes 4.0 s.
- Outlook's MSAL adds a hidden iframe for a silent sign-in. The iframe gets `oauthRedirect.html`. Its `msal-redirect-bridge` script sends the reply on a `BroadcastChannel`. MSAL waits 10 s for that message (`redirect_bridge_timeout`), then redirects the top page.
- In bare, no request went out for about 10 s. The parent page ran 2 rounds in that time. The iframe request started only when the 10 s timer fired.
- Cause: the live driver (`Page::work`) calls a page again only when `live_work` says that there is work. `Kids::live_work` did not count 3 states of a child page:
  - a slot whose document is not requested yet (`dirty`: an iframe was added, or its `src` changed);
  - a wire in the port hub (a `BroadcastChannel` message from a child that waits for the parent);
  - a child whose document is in, but that has not had its first turn (`State::Run`, `started == false`).
- The virtual clock path was not affected: it gives the children a turn in each idle step.

## Fix

- `Kids::live_work` gives `Some(true)` in each of the 3 states. The next tick then starts the load, takes the wire or runs the first turn.
- Cost: one flag check per slot and one hub counter read per tick, only on a page that has a child page or a slot.

## Check

- Test file `page_live_iframe_tests.rs`, 4 tests on the live `Page::tick` path. All 4 fail before the fix. Each of the 3 checks is needed: without the hub check, the broadcast test fails.
- `t`: pass.
- Cost rows, archMachine, glibc, 3 runs each, base b1fc3774: `navigation_load frames` and `session_load` (6 rows). The maximum is +0.039% (`navigation_load frames 1`). No row is over the rule.
- Live Outlook, archMachine, open to login form, no login (`open`, then `expect input[name=loginfmt]`):
  - before (b1fc3774): 14.5 s, 1 run;
  - after: 6.6 s, 3.5 s, 3.6 s (3 runs);
  - Chrome 141 headless, fresh profile: 4.0 s, 1 run.
- Not tested: the full Outlook login journey and Teams on this change. Not tested on Windows.
