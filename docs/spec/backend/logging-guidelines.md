# Logging Guidelines

> How logging is done in this project.

---

## Overview

<!--
Document your project's logging conventions here.

Questions to answer:
- What logging library do you use?
- What are the log levels and when to use each?
- What should be logged?
- What should NOT be logged (PII, secrets)?
-->

(To be filled by the team)

---

## Log Levels

<!-- When to use each level: debug, info, warn, error -->

(To be filled by the team)

---

## Structured Logging

<!-- Log format, required fields -->

(To be filled by the team)

---

## What to Log

For provider catalog discovery, diagnostics may include the Hya provider id,
safe error class, HTTP status code, and an endpoint origin/path only after its
query and userinfo have been removed. Keep messages bounded. Provider failure is
non-fatal when another provider resolves.

---

## What NOT to Log

Never log credentials, Authorization/API-key/account/session headers, credential
values or fingerprints, URL userinfo/query secrets, provider response bodies,
model payload dumps, or foreign config contents. Catalog API status fields are
the closed non-secret enums `source`, `auth`, and `result`; do not serialize an
internal error string in their place.
