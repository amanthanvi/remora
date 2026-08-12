# Plan 020: Browser preview and automation

Status: **TODO**
Tracker: [#30](https://github.com/amanthanvi/remora/issues/30)
Baseline: Plan 018 reviewed Link commit
Depends on: Plan 018

## Scope and ownership

A separate Link controller owns pinned system Chromium, exact Host+Project
profiles, Thread pages, Working Copy port attribution, bounded CDP methods,
evidence, `.remora-rec` recording, idle restore, grants, and profile clearing.

## Contract and acceptance

Never disable the sandbox. Allow only the proven loopback origin/port and
same-origin websocket. Apply the accepted DOM/screenshot/log/timeline/recording
caps and 30-second maximum command deadline. Test port/origin escape, sandbox,
downloads, permissions, choosers, oversize, timeout, clearing, and restore.

Rollback the module and capability advertisement together. STOP when process/
port association is uncertain, Chromium cannot be sandboxed, or a generic CDP
or network proxy endpoint would be required.
