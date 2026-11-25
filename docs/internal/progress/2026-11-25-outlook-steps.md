# Outlook login journey

- Adds `docs/internal/outlook/outlook-login.steps`.
- The journey logs in to Outlook web and waits for `[role=main]`.
- Run 1 on main 979eb9b7 (archMachine): the inbox renders in 24 s. Peak RSS is 387 MiB.
- Docs only. No code change, so no cost rows and no test262 counts.
