# Security policy

duscope is pre-1.0: 0.1.0 scans trees and lists what takes their space. It
writes nothing to the trees it scans and opens no socket. This policy is a
placeholder. P2-SEC-01 replaces it before the MCP server lands, with the threat
model (the untrusted inputs duscope reads and the rule for each), the supported
versions and the security-release procedure.

## Reporting a vulnerability

Report a suspected vulnerability privately, through GitHub's private
vulnerability reporting:
[open a report](https://github.com/kawadeomkar/duscope/security/advisories/new)
(the repository's Security tab, then "Report a vulnerability"). Please do not
open a public issue, pull request or discussion about it.

Include what you can: the output of `duscope --version`, the operating system
and filesystem, the steps or the input that reproduce the problem, and the
impact as you see it.

The report opens a private advisory that only you and the maintainer can see.
A confirmed vulnerability is fixed in private, released with a `Security` entry
in the changelog, and disclosed by publishing the advisory, with a CVE requested
through GitHub. The disclosure window is 90 days from the report: the advisory
is published with the release that fixes the vulnerability, and no later than
90 days after the report.

## Supported versions

From 0.1.0 until 1.0, only the latest minor release receives security fixes.
0.0.1, the scaffold that printed its version, receives none.
