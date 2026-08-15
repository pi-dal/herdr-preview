# Security

Herdr Preview reads repositories and uses the authenticated forge CLIs for its read-only PR tab.
It never writes to the worktree, index, branches, or forge. Files-only retains its selected root
as a descriptor capability and opens directories and file content relative to it without following
symlinks. Report any violation of those boundaries as a security issue.

Report fork-specific vulnerabilities privately through
[Herdr Preview security advisories](https://github.com/pi-dal/herdr-preview/security/advisories/new)
rather than a public issue.

Herdr Preview inherits its review engine from
[persiyanov/herdr-reviewr](https://github.com/persiyanov/herdr-reviewr). If a vulnerability also
affects upstream, coordinate disclosure with the upstream project's security channel after
reporting it to this fork.
