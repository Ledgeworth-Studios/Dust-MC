<!-- Transcribed from the planning documents at Phase 0.1. The plan is the
     working copy; this file is the record. Where they differ, this one is
     what the project decided. -->

# D2 — License: GPL-3.0

**Status:** Accepted, 2026-08-21. Supersedes a same-day decision for MIT.

**Context.** The project's stated goal is maximum access to existing implementations — plugins,
anti-cheats, servers — for reference and for incorporation. The repository shipped with
Apache-2.0 by default. The question asked was which license gives Dust access to the most code.

**The reasoning, because the intuition points the wrong way.** Licenses flow one direction: a
project may absorb code that is *no more restrictive* than itself. A permissive project can take
almost nothing; a copyleft project can take almost everything. The more restrictive license is
the one that opens doors inbound and closes them outbound.

**Ecosystem survey, verified 2026-08-21.**

| Project | License | Available to a GPL-3.0 Dust |
| --- | --- | --- |
| Paper (inherits from Spigot and CraftBukkit) | GPL-3.0 | ✅ |
| Folia — reference for regionised threading | GPL-3.0 | ✅ |
| Velocity — reference for modern forwarding | GPL-3.0 | ✅ |
| Pumpkin — most mature Rust server | GPL-3.0 | ✅ |
| PatchBukkit — working JVM plugin bridge | GPL-3.0 | ✅ |
| Grim — reference modern anti-cheat | GPL-3.0 | ✅ |
| packetevents | GPL-3.0 | ✅ |
| EssentialsX | GPL-3.0 | ✅ |
| WorldEdit | LGPL-3.0 | ✅ |
| LuckPerms, Waterfall, FerrumC, Valence | MIT | ✅ |
| BungeeCord | BSD-style | ✅ |
| SteelMC | AGPL-3.0 | ⚠️ combinable under §13 only |

**Decision.** GPL-3.0, copyright Ledgeworth Studios. Dust is and remains open source.

**Why not AGPL-3.0, which is more restrictive still.** AGPL-3.0 can absorb everything GPL-3.0 can
plus AGPL-3.0 code, and the whole of what that adds in this ecosystem is SteelMC — pre-alpha,
465 stars, no plugin or mod compatibility by its own README. Against that, AGPL §13 requires
anyone who modifies the software and exposes it over a network to offer users the modified
source. A Minecraft server is the textbook case that clause was written for, and networks treat
their private patches as their differentiator. It taxes precisely the audience expected to adopt
this, for one pre-alpha dependency. GPL-3.0 §13 still permits *combining* with AGPL-3.0 code
should that ever become worthwhile; the network obligation then attaches to that portion only.

**Consequences.**
- Nearly every implementation worth learning from is legally available, including the two hardest
  problems on the roadmap: the JVM plugin bridge and the anti-cheat.
- Anyone distributing a modified Dust must release those changes under GPL-3.0. No closed-source
  edition, by Ledgeworth Studios or anyone else.
- Dust cannot be incorporated into an MIT or proprietary codebase. Accepted.
- This is the ecosystem norm — Paper, Spigot, Folia and Velocity are all GPL-3.0 — so it will
  read as unremarkable rather than as a statement.
- **`NOTICE` becomes mandatory.** Permission arrives with an attribution obligation attached.
- The license is settled before contributors exist. Relicensing later needs every contributor's
  agreement.
