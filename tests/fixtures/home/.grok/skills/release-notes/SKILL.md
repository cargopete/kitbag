---
name: release-notes
description: Draft release notes from the git log since the last tag. Use when asked for release notes.
---

Run `git log $(git describe --tags --abbrev=0)..HEAD --oneline` and group the changes.
