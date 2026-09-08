---
status: "accepted"
date: 2026-09-08
deciders: ["AI-RCOV Team"]
consulted: []
informed: ["Contributors", "AI Assistants"]
---

# 1. Record Architecture Decisions with Markdown Architectural Decision Records (MADR)

## Context and Problem Statement

AI-RCOV is a high-performance radar coverage computation system implemented as a Rust workspace. It handles complex geometric and physical algorithms (SRTM terrain processing, azimuthal equidistant projections, DDA ray traversal, line-of-sight analysis) and high-throughput binary serialization formats (`.rcov` and `.rhgt`).

As the project evolves, critical architectural decisions are made regarding data layout, memory allocation, algorithm design, crate boundaries, and API stability. Without a systematic method to record *why* decisions were made, the team and AI coding assistants risk:
- Losing the rationale behind subtle engineering choices or performance optimizations.
- Accidentally re-litigating settled discussions.
- Making architectural changes that inadvertently violate foundational constraints.

We need a lightweight, version-controlled mechanism to document and preserve architectural decisions.

## Decision Drivers

* **Version Control Co-location**: Decision records must reside in the Git repository alongside the code so that code changes and architectural rationale evolve together.
* **Human & AI Friendliness**: Records must be plain Markdown, easy to write and review in pull requests, and parseable by AI coding assistants.
* **Low Friction & Lean Structure**: The format should be clear and concise, avoiding heavy bureaucratic overhead while ensuring essential forces and trade-offs are articulated.
* **Tooling Support**: Standardized syntax enabling automated index generation (such as `adr-log` or static site generators).

## Considered Options

* **Option 1: [MADR (Markdown Architectural Decision Records)](https://github.com/adr/madr)**
* **Option 2: Michael Nygard's Classic ADR Format**
* **Option 3: Ad-hoc Documentation** (in Git commits, PR descriptions, or unversioned wiki)
* **Option 4: Monolithic `docs/ARCHITECTURE.md` Updates Only**

## Decision Outcome

Chosen option: **"Option 1: MADR (Markdown Architectural Decision Records)"**, because:
- It uses standard Markdown with optional YAML frontmatter, making it natively renderable on GitHub, in IDEs, and directly understandable by LLM/agent tools.
- It explicitly structures decision drivers, considered options, pros/cons, and consequences, promoting rigorous reasoning.
- It is supported by a large open-source ecosystem (including [adr-log](https://github.com/adr/adr-log) and npm/cli generators).
- It provides clear distinction between immutable past decisions and superseding changes.

### Consequences

* **Good**: Architectural decisions are transparent, searchable, and preserved in Git history.
* **Good**: Pull requests introducing architectural changes include a corresponding ADR file, serving as a focal point for architectural review.
* **Good**: AI pair programmers can check `docs/adr/` before designing features or refactorings, avoiding drift from core principles.
* **Bad/Effort**: Requires writing discipline when introducing architectural modifications or significant new crates.

## Validation

The process will be validated by:
1. Verifying that `docs/adr/template.md` and `.agents/rules/architecture-decisions.md` are accessible in the workspace.
2. Reviewing subsequent architectural pull requests to ensure relevant decisions include an ADR.

## Pros and Cons of the Options

### Option 1: MADR (Markdown Architectural Decision Records)

* Good: Standard Markdown with rich, structured sections (Context, Drivers, Options, Outcome, Consequences, Pros/Cons).
* Good: Supported by automated tools like `adr-log` for generating indices.
* Good: Clear template (`docs/adr/template.md`) that guides authors without overwhelming them.
* Bad: Slightly more structured than Nygard's format, requiring a bit more initial typing.

### Option 2: Michael Nygard's Classic ADR Format

* Good: Very brief (Title, Context, Decision, Status, Consequences).
* Good: Widely recognized.
* Bad: Does not explicitly structure the list of considered alternatives or decision drivers, often leading authors to omit alternative solutions.

### Option 3: Ad-hoc Documentation (PR descriptions, Git commits, Wiki)

* Good: No immediate setup required.
* Bad: Dispersed across commits or external websites; easily forgotten or disconnected from active code.
* Bad: Difficult for new contributors and AI agents to systematically discover prior architectural constraints.

### Option 4: Monolithic `docs/ARCHITECTURE.md` Updates Only

* Good: Centralized single-page overview of current state.
* Bad: Documents the *current* state of the system, but completely obscures *why* past trade-offs were chosen or what alternatives were rejected.

## More Information

* [MADR GitHub Repository](https://github.com/adr/madr)
* [ADR GitHub Organization](https://adr.github.io/)
* Template in this repository: [docs/adr/template.md](file:///h:/repos/jblestang/AI-RCOV/docs/adr/template.md)
* Architecture Decisions Rule: [.agents/rules/architecture-decisions.md](file:///h:/repos/jblestang/AI-RCOV/.agents/rules/architecture-decisions.md)
