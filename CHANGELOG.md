# 0.36.52

## ask_user merges question: one canonical batch question tool

The two overlapping question tools collapse into one. `ask_user` is now
the canonical batch tool (previously `question`'s shape): a
`questions[]` array where each item carries `question`, `header`,
`options: [{label, description}]` (empty list = free text, with
optional `default`), `multiple`, and `allow_custom` (legacy `custom`
spelling still parses).

- Results carry structured per-question entries in `metadata.answers`
  (`{question, answer: [chosen values], cancelled}`) alongside the
  human-readable answer line; unanswered questions render as
  `Unanswered`.
- Plane failures now surface as tool errors instead of being silently
  swallowed as empty answers.
- The old single-shot `ask_user` schema (`kind`/`options`/`default`
  top-level) is removed. The `question` spelling remains dispatchable
  as a hidden non-advertised alias with identical batch semantics.
- Canonical advertised tool count: 27 → 26.
