---
name: release-notes
description: Draft release notes from the git history since the last tag. Use when the user asks for a changelog or release notes.
---

# Release notes

1. Find the previous tag: `git describe --tags --abbrev=0`.
2. List changes since then: `git log --no-merges --pretty='%s (%h)' <tag>..HEAD`.
3. Group them under **Added**, **Changed**, **Fixed** and **Removed**; drop chores.
4. Write one line per user-visible change, in the imperative mood.
