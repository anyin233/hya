# 0.33.30

- Fixed `permission.model: allow` still prompting the CLI for tool and external-directory checks; allow mode now auto-approves resource asserts while still honoring explicit Deny rules.
- Accepted `permission.mode` as an alias for `permission.model`, and lowercase `allow`/`ask`/`deny` on rule effects so config casing matches the model field.
