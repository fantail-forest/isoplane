# Security

Report a vulnerability in Isoplane privately through GitHub:
**Security → Report a vulnerability** on this repository, or
https://github.com/fantail-forest/isoplane/security/advisories/new. If you
prefer email, write to security@fantailforest.com or security@isoplane.dev.

Do not open a public issue for a security problem. We acknowledge reports
within five business days and keep you informed until the issue is resolved.

The SDK runs applications in a local sandbox, and the self-hosted daemon serves
operator-deployed applications. Reports are in scope when an application can
escape its documented sandbox or access another application's state, when the
SDK, CLI or daemon mishandles untrusted input, or when configuration, secrets
or deployment artifacts are exposed contrary to the documented protections.
