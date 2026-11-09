# Security Policy

archivix parses untrusted input (archives from anywhere), so security reports
are taken seriously.

## Reporting a vulnerability

**Please do not open a public issue.** Report privately through GitHub's
[private vulnerability reporting](https://github.com/swapnil-saal/archivix/security/advisories/new)
("Security" tab → "Report a vulnerability").

Include the archivix version, OS, the command, and a sample archive that
reproduces the problem, if possible. You can expect an acknowledgement within a
few days and a fix or mitigation plan once the issue is confirmed. We'll credit
you in the advisory unless you prefer otherwise.

## In scope

* Path traversal / zip-slip: any way for an archive to write outside the
  extraction directory, including via symlinks or hardlinks
* Memory-safety issues, crashes or unbounded resource use (zip bombs,
  decompression bombs) triggered by crafted archives
* Integrity checks that can be bypassed (CRC or stream checksums not verified)
* The MCP server performing actions beyond what a tool call requests

## Supported versions

Security fixes go into the latest release.

## Notes

The bundled C libraries (zstd, liblzma, libdeflate, unrar) are compiled from
upstream sources by their Rust wrapper crates. Vulnerabilities in those
libraries should also be reported upstream; we will update the dependency.
