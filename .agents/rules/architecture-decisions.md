---
trigger: always_on
---

# Architectural Decisions (ADR)
- **Standard**: Follow [MADR](https://github.com/adr/madr) template in [`docs/adr/template.md`](file:///docs/adr/template.md).
- **Location**: Store records in [`docs/adr/NNNN-kebab-case.md`](file:///docs/adr/) (4 digits, zero-padded, sequential).
- **Trigger**: Required for changes to crates, binary formats (`.rcov`/`.rhgt`), core algorithms (LOS/DDA/projection), storage, concurrency, or major dependencies.
- **Agent Policy**: Before proposing or implementing architectural changes, inspect existing ADRs in `docs/adr/` and draft a new ADR using the template.
