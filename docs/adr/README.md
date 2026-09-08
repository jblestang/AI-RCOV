# Architecture Decision Records (ADR)

This directory contains the **Architecture Decision Records (ADR)** for the `AI-RCOV` project, following the [MADR (Markdown Architectural Decision Records)](https://github.com/adr/madr) standard.

## Index of Decisions

* [ADR-0001: Record Architecture Decisions with Markdown Architectural Decision Records (MADR)](./0001-record-architecture-decisions.md)
* [ADR-0002: Hierarchical H3 Hexagonal Cell Export from Native Coverage Grids](./0002-h3-hierarchical-coverage-export.md)

---

## How to Propose a New ADR

1. **Copy the template**:
   ```bash
   cp docs/adr/template.md docs/adr/NNNN-short-descriptive-title.md
   ```
   Replace `NNNN` with the next 4-digit sequential number (e.g. `0002`).

2. **Fill in the details**:
   - Set status to `proposed` (or `draft`).
   - Describe the context, forces/drivers, and options considered.
   - Explain why the chosen option was selected, and list its consequences (both benefits and trade-offs).

3. **Review**:
   - Submit a pull request including the new ADR file alongside or prior to the implementing code.
   - Once discussed and approved by maintainers, update status to `accepted`.

4. **Update this Index**:
   - Add a link to the new ADR in the "Index of Decisions" section above.
   - Alternatively, you can use [`adr-log`](https://github.com/adr/adr-log) to generate or update the index:
     ```bash
     npx adr-log
     ```

## Resources

- [MADR Template](./template.md)
- [MADR Specification & Guidelines](https://github.com/adr/madr)
- [Project Architecture Decisions Rule](../../.agents/rules/architecture-decisions.md)
