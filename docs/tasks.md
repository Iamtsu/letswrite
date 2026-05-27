# letswrite — Task List

Master plan for building letswrite, a Rust/Iced book-writing app. Phased so each phase is independently dogfoodable on the real novel ([The-Threshold](/home/tsu/Projects/private/The-Threshold)).

## Phase 0 — Foundations
- [x] **#1 P0.1** Bootstrap Cargo workspace + Iced shell
- [x] **#2 P0.2** Project scaffolding: settings, logging, error types — *blocked by #1*
- [x] **#3 P0.3** SQLite schema + migrations — *blocked by #2*
- [x] **#4 P0.4** Filesystem-first document model — *blocked by #3*

## Phase 1 — Editing (first dogfoodable slice)
- [x] **#5 P1.1** Markdown editor pane with frontmatter handling — *blocked by #4*
- [x] **#31 P1.1b** Markdown syntax highlighting (3 themes incl. color-blind safe) — *blocked by #5*
- [x] **#32 P1.1c** Highlight direct-speech quotes in editor — *blocked by #31*
- [x] **#33 P1.1d** Ctrl+scroll to change editor font size — *blocked by #5*
- [x] **#34 P1.1e** Fix highlight disappearing after layout reflow — *blocked by #31*
- [x] **#6 P1.2** Markdown preview mode (toggle) — *blocked by #5*
- [x] **#7 P1.3** Project / file navigation sidebar — *blocked by #4*
- [ ] **#8 P1.4** Obsidian vault importer (The-Threshold) — *blocked by #4, #7*
- [ ] **#9 P1.5** Snapshots (per-document versioning) — *blocked by #5*
- [ ] **#10 P1.6** Distraction-free / focus mode — *blocked by #5*
- [ ] **#11 P1.7** Word count goals — *blocked by #5*

## Phase 2 — AI assistant (non-blocking, context-aware)
- [ ] **#12 P2.1a** Design AI Assistant abstraction (Provider + Agent traits) — *blocked by #2*
- [ ] **#30 P2.1b** Anthropic provider implementation — *blocked by #12*
- [ ] **#13 P2.2** Right-column assistant panel — *blocked by #5, #30*
- [ ] **#14 P2.3** Context builder for AI requests — *blocked by #5, #8, #13*
- [ ] **#15 P2.4** Quick-action prompts — *blocked by #13, #14*
- [ ] **#16 P2.5** Character hints panel — *blocked by #8, #13*
- [ ] **#17 P2.6** Entity mention detection + confirmation flow — *blocked by #8*

## Phase 3 — Structural views
- [ ] **#18 P3.1** Character overview & editor — *blocked by #8*
- [ ] **#19 P3.2** Location overview & editor — *blocked by #18*
- [ ] **#20 P3.3** Scene cards / corkboard view — *blocked by #8*
- [ ] **#21 P3.4** Plot/timeline view — *blocked by #20*
- [ ] **#22 P3.5** Relationships graph — *blocked by #18, #19*
- [ ] **#23 P3.6** Minimap of all characters (graphical) — *blocked by #16, #17*
- [ ] **#24 P3.7** Research / worldbuilding notes — *blocked by #7*

## Phase 4 — Export, packaging, polish
- [ ] **#25 P4.1** Export: Markdown bundle — *blocked by #6*
- [ ] **#26 P4.2** Export: EPUB — *blocked by #25*
- [ ] **#27 P4.3** Export: PDF + DOCX — *blocked by #25*
- [ ] **#28 P4.4** Packaging: AppImage / .app / Windows installer — *blocked by #1*
- [ ] **#29 P4.5** Onboarding + sample project — *blocked by #8, #18, #20*

---

## Critical path

`#1 → #2 → #3 → #4 → #5 → (#6 || #7) → #8 → #14 → assistant works on real novel`

AI side: `#2 → #12 (abstraction) → #30 (Anthropic) → #13 (UI) → #14 (context)`

After #8 (Obsidian import) most of phases 2 and 3 unlock in parallel.

## Notes
- See memory: project decisions and conventions live in `/home/tsu/.claude/projects/-home-tsu-Projects-private-letswrite/memory/`
- The full task tracker (TaskList) is the source of truth during a session; this file is the cross-session checkpoint.
