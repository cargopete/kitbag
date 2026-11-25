---
description: Checklist for reviewing changes that touch auth, secrets or user input
alwaysApply: false
---

When a change touches authentication, secrets or untrusted input, check:

1. Secrets come from the environment or a secret store, never from source.
2. Input is validated at the boundary, and errors don't leak internals.
3. New dependencies are maintained and pinned.
