# DNS: wait 5 s for each try

## Problem

Two of about six live Teams runs failed before the page loaded. The error was "Resource temporarily unavailable (os error 11)" from the DNS lookup.

bare waited 2 s for each DNS try. systemd-resolved can take more than that on a cold name when the upstream server is slow. On archMachine (2026-11-25), 3 of 12 cold lookups took 1.4 s to 2.08 s. Each retry opens a new socket, so a late reply to the first try is lost.

## Change

`crates/net/src/dns.rs`: `TIMEOUT` is now 5 s. This is the glibc value (`RES_TIMEOUT`). A fast reply is not slower. Only a server that does not answer makes the failure later.

## Tests

- New: `crates/net/src/dns_slow_tests.rs`, `a_reply_after_two_and_a_half_seconds_is_used`. It failed at 2.04 s before the change. It passes now.
- `t`: pass.

## Cost

No cost rows. The change touches only the wait on a slow DNS reply.

## Not tested

- A live Teams run with a slow resolver after the change.
- The case where no server answers. The failure now comes after 10 s for each server (2 tries of 5 s), not 4 s.
