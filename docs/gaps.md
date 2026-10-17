# Gaps: where LightCraft falls short of Lightroom

> **Last reviewed:** 2026-10-11 · **Last updated:** 2026-10-17 · **Change:** minor (CR.58: local background media revisions; whole-library batch progress, activity-aware polling and focused cloud controls; native cloud on/off transitions and permanent local conversion; generated demo originals included in cloud mode; automatic whole-library cloud mode and one popup settings action; guarded cloud-library erasure and empty-bucket initialization; corrected cloud popup icons and compact layout; cloud popup redesign; unsupported migration capability requires update before takeover; cloud library control envelope, maintenance-gated format upgrades and epoch rejoin; typed synchronization failure and bounded transient retry; maintained personal browser signer and deployment/capability documentation; shared original byte/pixel admission and explicit browser decode limits; browser cache eviction, catalog reservation and quota recovery; live offline photo/album policy and empty albums; separate current cloud contracts and dated acceptance; pure upload readiness, publication and acknowledgement decisions; explicit portable cloud schemas and typed reference boundaries; immutable asset manifests and shared device-local blob rebinding; consumed append checkpoints and resumable budgeted integrity audits; indexed cloud identities, changed-entity materialization and synchronization journal deltas; incremental archive proof planning and shared witness caches; protocol-independent destination binding and credential rotation; durable managed-import retry identities and independent recovery; supporting XMP preservation across managed imports and revisions; browser export source leases and shared byte admission; separate library creation/join and validate append capacity before publication; bounded browser head replay and measured native catch-up memory; bounded native head-history recovery and automatic checkpoints; pure shared folder/stack projection with generated convergence invariants; separate cloud upload stages and bounded fresh transfer evidence; logical field-aware cloud undo and redo; explicit subsystem cloud health and structural-conflict attention; durable upload preparation errors and retry actions; durable local receipt finalization without media retransfers; shared stack ownership independent of local overlays; device-local smart rules and explicit empty selections; retained smart-album references and unavailable-rule semantics; both-protocol writer ownership and same-path restore protection; serialized metadata publication and monotonic completion; unchanged head polls avoid catalog writes; explicit entity setup/edit limits; capacity usage and early warnings; authenticated receipt history compaction; cloud queue popup, persisted pause and sync filters; coordinated desktop/browser migration workflow; append-only B2 synchronization; CR3 entry names the EOS R7 C-RAW fix and the refused CRX variants; CI and Windows / Linux entries note the 0.6.0 riscv64 build; camera colour: tone curve from black-and-white camera JPEGs; Compare and Reference get sharp zoom windows; gap 4 shrinks; gap 14: the HDR switch is read from crs XMP; gaps 2 and 10: EOS R5 Mark II CR3 verified) · **Target:** Adobe Lightroom Classic 15.5 / Lightroom 9.6

Every known shortfall, one entry each, ranked by user impact (how many Lightroom users hit it, and
how hard it blocks them), then by effort. **Agents pick work from the top** (the first entry nobody is
working on). When you close a gap, delete or shrink its entry, update its rows in
[parity-checklist.md](parity-checklist.md), and the numbers in [target-app-parity.md](target-app-parity.md)
and [ROADMAP.md](../ROADMAP.md) if they move.

Hours are Opus 5.5 agent wall-clock hours (see [target-app-parity.md](target-app-parity.md#how-hours-are-calibrated)).
`B` marks a gap that blocks **beta**.

## Ranked

| # | Gap | Evidence | User impact | Hours | Doc |
|---:|---|---|---|---:|---|
| 1 B | **Camera colour is estimated, not measured** (LR-PROF-CAMERACOLOR). Raws start from a per-file fit to the camera JPEG, 4 bundled profiles or 52 spectral matrices; others use a neutral matrix (a black-and-white camera JPEG gives its tone curve only, #577) | ΔE vs camera JPEG 2.9–10 after fitting (raw-parity.md); 0 chart-calibrated models vs 1,500 Adobe Standard profiles | every raw shooter sees colour that is close but not Lightroom's; some get neutral-matrix colour | 50–90 | [raw-parity](raw-parity.md#colour) |
| 2 B | **CR3 beyond four verified bodies** (LR-IMP-FORMATS): `crx.rs` refuses vertical-tile C-RAW, partial-subband and other plane coding, multi-tile 0x200 and adaptive low-frequency gains, and those files fall back to the preview; CR3 sRAW / mRAW and HEIF CR3 too. Verified: M50 / R100 / R8 / R5 Mark II. EOS R7 C-RAW (odd-height QP map) decodes since a45c2b66 and R6 Mark II C-RAW is reported working on 0.6.0, but neither is verified yet (no corpus test or recorded check) | measured: Canon is the largest bucket (33% of photographers) and its share whose camera fully works is the lowest of the big four, 73%; 18 of 22 popular Canon bodies write CR3, 4 verified (raw-parity.md); R7 rule checked only on paired RAW / C-RAW captures ([cr3.md](cr3.md)) | +6.2 points of photographers whose camera works, the biggest single raw gain | 25–45 | [raw-parity](raw-parity.md#remaining-work-ranked-by-photographers-unlocked-per-hour) |
| 3 B | **Render fidelity unmeasured** (LR-BEHAV-RENDER-FIDELITY): tone, highlights, texture / clarity / dehaze, NR and sharpening are tuned by eye | no side-by-side suite; a retune needs a new process version (only V1 exists) | edits imported from Lightroom look different; the look "isn't Lightroom's" | 30–60 | [raw-parity](raw-parity.md), [process-versions](process-versions.md) |
| 4 B | **Original-pixel detail at 100%** (LR-VIEW-ZOOM, P0): Detail, Before/After, Compare and Reference are sharp at 1:1–8:1 (zoom windows, #323). Mask overlays and soft proofing still render the whole frame at the drawn size instead of a window: sharp up to the GPU texture side, but a full-size render per edit at 1:1 | checklist row 🟡 | editing with a mask overlay or soft proof on at 1:1 is slow on large files, and blurry past the texture side | 4–8 | [ui-parity](ui-parity.md) |
| 5 B | **AI masks: Subject, Sky, People, Background, Landscape** are heuristics; Object / Describe run SAM 3 but its download has no mirror configured | `crates/segment`; ai-masks.md; MASK rows 67% | masking is how most Lightroom users edit today | 40–70 + model decision | [target-app-parity](target-app-parity.md#ai) |
| 6 B | **Compressed ORF** (every Olympus / OM body since ~2008) | measured: 66 of 78 Olympus / OM models preview-only; `orf.rs` refuses; OM bucket 3% of photographers, 3% of them fully work | +2.6 points; best gain per hour of any raw gap (~0.2) | 8–15 | [raw-parity](raw-parity.md#compressed-orf-a-clean-room-starting-point) (the two Olympus patents that describe the method) |
| 7 | **Nikon High Efficiency NEF** (HE / HE★ on Z 8, Z 9, Z 6III, Z f, Z50II, Z5II when set to HE) | named and preview only (#193); needs a pure-Rust JPEG XS decoder | +1.1 points (only shots taken in HE; lossless works on the same bodies); low gain per hour (~0.04), so not a beta blocker | 20–40 | [raw-parity](raw-parity.md) |
| 8 B | **No lens profile database** (LR-EDIT-OPTICS-PROFILE): only DNG opcodes, RW2 in-file distortion and Sony ILCE-7RM4A distortion | Adobe ships 3,635 lens profiles; we never use them | distortion / vignetting correction is a default step for many | 40–80 + calibration shots | [raw-parity](raw-parity.md#lens-profiles) |
| 9 B | **Tests don't run in CI on macOS, Windows or Linux x64**: only FreeBSD runs `cargo test`; the others build packages | `.github/workflows/` (freebsd, packaging-lint, release, windows-arm64); the linux-riscv64 release job added in 0.6.0 (#844) runs only `lightcraft-cli --version` under QEMU | regressions reach users on the main platforms; `cargo xtask ci` runs only locally | 6–12 | [target-app-parity](target-app-parity.md#by-dimension) |
| 10 B | **Per-model verification thin, plus cheap per-body decode fixes**: 235 of 1,446 Adobe models verified (16%), ~42% of photographers on a verified body; Nikon 5 bodies verified, Canon 12. Cheap fixes: Z 6II packed 14-bit NEF (#769, PR #671), Sony A100 / F828 regression (#702), DSLR-A200…A390 / A850 12-bit ARW (#535) | measured: [raw/coverage.tsv](raw/coverage.tsv) | per-body bugs (levels, crops, WB) surface only from users | 25–45 | [raw-parity](raw-parity.md#by-maker-measured) |
| 11 | **Enhance: Raw Details, Super Resolution; AI denoise without shippable weights or X-Trans** | ENH rows 0%; denoise Bayer only, RawNIND weights GPL and opt-in | Fujifilm users get no AI denoise; nobody gets it out of the box | 30–60 + model decision | [raw-parity](raw-parity.md#demosaic-levels-crops-and-lens-metadata) |
| 12 | **Content-aware / generative remove, detect distractions, reflections removal** | REM rows 75%; patch synthesis missing | cleanup work goes back to Photoshop | 25–45 (+ generative model) | [target-app-parity](target-app-parity.md#ai) |
| 13 | **Lightroom catalog migration is one-way and approximate**: unsupported Adobe profiles / AI settings archived, history kept as data | lightroom-catalog-import.md | switching users lose some looks | 8–12 | [file-format-parity](file-format-parity.md) |
| 14 | **HDR display** (EDR / Windows HDR); HLG; the headroom limit and SDR rendition from `crs` XMP (the HDR switch, `HDREditMode`, is read) | HDR rows 70%; LR-VIEW-HDR-DISPLAY ⬜ | HDR editing works blind on HDR monitors | 12–25 | [hardware-parity](hardware-parity.md) |
| 15 | **Print module**: custom packages, printer colour management (contact-sheet PDF only) | LRC-PRINT rows | Classic users who print | 25–40 | [target-app-parity](target-app-parity.md#features) |
| 16 | **Map view, Book, Slideshow module, publish services** | LRC-MAP / BOOK / SS rows ⬜ | Classic users; publish services matter for Flickr / SmugMug workflows | 60–100 | [target-app-parity](target-app-parity.md#features) |
| 17 | **Tethered capture** | ⬜ | studio photographers | 25–45 + cameras | [hardware-parity](hardware-parity.md) |
| 18 | **Video**: playback, trim, global edits, export | VID rows 0% | users who mix stills and clips | 25–45 | [target-app-parity](target-app-parity.md#features) |
| 19 | **Plug-in ecosystem**: no Lightroom SDK equivalent (export / publish plug-ins, metadata plug-ins) | ⬜ | pro workflows built on plug-ins (LR/Enfuse, Jeffrey's exporters, Negative Lab Pro) | 40–80 + owner decision | [target-app-parity](target-app-parity.md#by-dimension) |
| 20 | **Long-tail raw formats**: compressed SRW, ARQ pixel shift, IIQ, Hasselblad compression, X3F, CRW, MRW, ERF, KDC, DCR, MEF, GPR, old FinePix | raw-parity.md | small user share each | 45–85 | [raw-parity](raw-parity.md) |
| 21 | **Localization**: 9 of 15 shipped languages partial (74–99% of catalog messages), lower-layer errors English; Hindi, Arabic, Indonesian, Korean, Vietnamese absent; no RTL or complex shaping | measured from catalogs | non-English users see mixed English | 55–90 (shipped to full); 145–255 (five new) | [localization-parity](localization-parity.md) |
| 22 | **Pen pressure**, OS monitor profiles, follow-window colour | hardware-parity.md | brush users with tablets; multi-monitor setups | 11–22 | [hardware-parity](hardware-parity.md) |
| 23 | **Accessibility** audit with screen readers; canvas tools keyboard-reachable | LR-BEHAV-ACCESS 🟡 | blind and low-vision users | 10–20 | [ui-parity](ui-parity.md) |
| 24 | **Brush live preview and masking feel** (#517, #518); Feather / Edge mask refinement (15.5) | issues | masking feels slower than Lightroom | 8–12 | [ui-parity](ui-parity.md) |
| 25 | **File writes**: lossy DNG, Render to DNG; AVIF import (PSD export landed with Edit in PhotoCraft, #953) | file-format-parity.md | interchange edge cases | 21–37 | [file-format-parity](file-format-parity.md) |
| 26 | **Performance at scale**: 85k-photo library opens in 1.7–4.7 s; 100k grid test pending; colour NR at half resolution | ROADMAP log, gpu-pipeline.md | large-catalog users | 15–30 | [target-app-parity](target-app-parity.md#by-dimension) |
| 27 | **Windows / Linux runtime coverage**: Windows installer UI unverified on Windows (#79); GPU path proven on Apple only; the Linux riscv64 tarball (new in 0.6.0, #844) is cross-compiled and never run beyond a QEMU smoke test | ROADMAP, issues, `release.yml` | non-Mac users find bugs first | 15–30 | [hardware-parity](hardware-parity.md) |
| 28 | **Small Classic shortcuts and keys**: rating `[` `]`, flag cycling, filter-bar keys, F1 help, panel keys (Tab, F5–F8) | [ui-parity](ui-parity.md#shortcuts) | Classic muscle memory | 4–8 | [ui-parity](ui-parity.md) |

Beta-blocking gaps (B) total **~230–430 h**; with the stability and platform work they need around them,
**~340–600 h** to beta (incl. 40–80 h for the ~92 open core-path user issues) (see [target-app-parity.md](target-app-parity.md#stage)).

## Gaps by kind

- **Feature:** 5, 8, 11, 12, 13, 15, 16, 18, 19
- **UI / UX:** 4, 23, 24, 28
- **File format:** 2, 6, 7, 10, 13, 20, 25 (raw ones in [raw-parity.md](raw-parity.md))
- **Hardware:** 14, 17, 22, 27
- **Localization:** 21
- **Quality / fidelity:** 1, 3
- **Stability and platforms:** 9, 26, 27

## Cloud storage follow-up

Cloud storage remains outside the ranked Adobe cloud-service parity scope. It closes
none of the ranked gaps or beta blockers above. The native/browser direct-bucket
implementation has durable causal synchronization, independent media transfer,
exact publication acknowledgements, conflict/tombstone recovery, immutable asset
manifests, supporting XMP and device-local verified caching. These are feature claims,
not proof of provider compatibility, camera fidelity or large-library responsiveness.
CUX.12 adds whole-library preparation counts, activity-aware 60-second/15-minute scans
and focused popup actions; provider/live-library validation remains separate.
CR.58: Local-only background revisions keep their typed completion context without permitting cloud enrollment.

[Current behavior and remaining limits](cloud-storage-implementation.md) is the
operational reference; [cloud-sync-protocol.md](cloud-sync-protocol.md) and the
[browser adapter contract](browser-cloud.md) specify the protocol. The
[reliability checklist](cloud-reliability-tasks.md) records completion and verification evidence. Typed synchronization failures now distinguish bounded
transient retry from authentication, integrity, schema/identity and local recovery; status shows
the retry countdown or required action. Upgrade acquisition and takeover now require a
supported plan/source/target converter before remote writes; newer targets require an
application update. Other upgrade/cache review findings remain in CR.48–CR.55.
The [personal signer deployment recipe](browser-signer.md)
provides a maintained scoped CLI service and desktop/browser capability matrix. [Input limits](cloud-input-limits.md)
now share admission checks and status reporting; native rendering accepts 128 MP and browser
original rendering 32 MP, with explicit smart-preview use for larger shared photos. Remote retention
now has explicit native orphan preview/apply; published history remains retained and
custom endpoints/B2 remain preview-only.
[../tasks.md](../tasks.md) remains the original milestone index. Native photo/album
offline policy now retains owner/quality intent, follows current membership and
immutable source revisions, and protects dependencies through catalog save/restart.
Browser caching now protects offline selections across libraries, reserves catalog headroom,
and retains verified session bytes when persistent storage is full. Explicit existing-photo adoption stages verified bytes and relinks original IDs and live
virtual copies without replacing their edits, album memberships or photo history.

[Dated acceptance](cloud-acceptance-history.md) distinguishes procedural native/browser
fixtures, measured scale/request costs and the earlier live B2 runs. B2 conditional
writes were unsupported at the tested endpoint; explicit append mode passed the dated
live tests. A later download/Class B account cap blocked further live rechecks.
Live AWS and a fresh post-change provider run remain unverified. Settings and library
data contain no provider secrets; browser signing requires a separately operated
scoped authorization endpoint. The optional managed metadata backend is deferred.

## Closed recently (kept for calibration)

- 2026-10-10: Nikon lossy-after-split NEF (#633), ORF 12.8 bpp (#651), HDR gain map / PQ AVIF / float
  TIFF export (#646), activity stack (#345), JPEG XL export (#247).
- 2026-10-09: EOS R7 C-RAW with an odd-height QP map (a45c2b66; part of gap 2), HEIC in release builds (#609), Samsung uncompressed SRW (#459), contact-sheet PDF (#563),
  Ukrainian UI (#564), Sony ILCE-7CR profile.
- 2026-10-08: CR3 CRX lossless and C-RAW (#279), Fujifilm compressed RAF (#282), AI RAW denoise (#384),
  Lightroom catalog import (#218), German, Spanish, Russian UI.
- 2026-10-07: RW2 every format (#215), faces recognition in pure Rust (#174).
- 2026-10-05: compressed NEF (#86). The older history is in [ROADMAP.md](../ROADMAP.md#progress-log).

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-16 | minor | CR.47: Refuse unsupported migration targets before ownership or output and derive activation compatibility from the converter; cloud remains OOS, counts unchanged |
| 2026-10-17 | minor | CR.42: Cloud library control envelope, separate reader/writer compatibility, head-gated format upgrades and epoch rejoin; cloud remains OOS, summary counts unchanged |
| 2026-10-17 | minor | CR.21: Typed native/browser failures, bounded transient retry and explicit recovery status; cloud remains OOS, summary counts unchanged |
| 2026-10-17 | minor | CR.20: Maintained personal signer and deployment/capability matrix; cloud remains OOS, summary counts unchanged |
| 2026-10-16 | minor | CR.10: Retain consumed append checkpoints and audit history in resumable bounded slices; cloud remains OOS, summary counts unchanged |
| 2026-10-16 | minor | CR.9: Index cloud identities, project changed entities and journal atomic synchronization deltas; cloud remains OOS, summary counts unchanged |
| 2026-10-14 | minor | Preserve nonportable smart rules and distinguish empty allowed sets with required reader versions; no summary count changes |
| 2026-10-14 | minor | Retain deleted/unresolved smart references across fresh bootstrap and fail closed under negation; no summary count changes |
| 2026-10-14 | minor | Track the supplied cloud reliability review and verified liveness, bulk-publication, structural-resolution and bounded-import corrections; remaining storage gaps stay open; no ranked parity gap closes |
| 2026-10-12 | minor | Record the cloud storage follow-up and foundation evidence; no ranked gap closes |
| 2026-10-12 | minor | Record verified cloud cache budgets, offline pin owners and local-copy removal; no ranked gap closes |
| 2026-10-12 | minor | Record cloud logical rename and album-folder organization with immutable original preservation; no ranked gap closes |
| 2026-10-12 | minor | Record explicit idle multipart cleanup, destination-bound receipts and native cancellation/retry; no ranked gap closes |
| 2026-10-12 | minor | Record cloud download priority, byte progress, cancellation and reconnect acceptance; no ranked gap closes |
| 2026-10-12 | minor | Record durable local cloud staging, atomic upload intent and duplicate-free recovery; uploads and synchronization remain open |
| 2026-10-12 | minor | Record verified resumable cloud uploads, conditional initial publication and native retry acceptance; no ranked gap closes |
| 2026-10-12 | minor | Record recipe-matched remote thumbnail browsing and editable-proxy/original refresh evidence; no ranked gap closes |
| 2026-10-11 | minor | Reviewed against main after 0.6.0: CR3 entry names the CRX variants `crx.rs` still refuses and the R7 / R6 Mark II C-RAW state (decoding, not verified, so the measured numbers don't move); CI and Windows / Linux entries note the riscv64 release build (#844); R7 fix added to Closed recently. Panel docking (UI.9, #869) closes no listed gap |
| 2026-10-11 | minor | Camera colour entry: raws with a black-and-white camera JPEG take its tone curve (#577); gap not closed |
| 2026-10-11 | minor | Gap 4: Compare and Reference views get zoom windows per pane (1:1 was a 2,560 px render magnified); overlays and soft proofing remain, 8–15 h → 4–8 h |
| 2026-10-10 | minor | Compressed ORF entry links the two Olympus patents that describe the method |
| 2026-10-11 | minor | Gap 14: `crs:HDREditMode` is read from XMP (#663); the headroom limit and SDR rendition still aren't |
| 2026-10-11 | minor | Gaps 2 and 10 shrink: EOS R5 Mark II CR3 decodes and is verified (fourth CR3 body, Canon 12 verified, 235 models, ~42% on a verified body); CR3 gain +6.5 → +6.2 points |
| 2026-10-10 | minor | RAW gaps re-measured model by model (raw-parity.md): CR3, compressed ORF and verification entries carry measured shares; HE NEF no longer a beta blocker (+1.1 points for 20–40 h); verification entry absorbs the cheap per-body fixes (#769, #702, A200 series) |
| 2026-10-10 | minor | Beta total raised to 340–600 h to include the ~92 open core-path user issues |
| 2026-10-10 | major | Created from the checklist's Top gaps (whose items 1–2 had been duplicated by a merge) and ROADMAP's Where we stand; re-ranked; added evidence, impact, hours and beta flags; added platform-CI, lens-database, HE NEF and localization gaps |
| 2026-10-13 | minor | Record managed DNG/XMP and external-editor revision acceptance; multi-device synchronization and browser transfers remain open, no ranked parity gap closes |
| 2026-10-13 | minor | Record managed DNG/XMP and external-editor revision acceptance; multi-device synchronization and browser transfers remain open, no ranked parity gap closes |
| 2026-10-13 | minor | Direct-bucket mutation synchronization, conflict versions and recovery passed native and CLI acceptance; browser adapters remain open; no ranked parity gap closes |
| 2026-10-13 | minor | Browser managed media and direct-bucket synchronization verified on OPFS/IndexedDB; no ranked Lightroom parity gap closes |
| 2026-10-14 | minor | Add cloud queue popup, persisted publication pause and sync-status filters; no ranked gap closes |
| 2026-10-14 | minor | Add coordinated migration dialog and asynchronous desktop cutover; no ranked gap closes |
| 2026-10-14 | minor | Record append-only B2 synchronization and live native/browser bucket acceptance; no ranked gap closes |

| 2026-10-14 | minor | Implement authenticated receipt-history maintenance across native/browser transports; no ranked parity gap closes |

| 2026-10-14 | minor | Expose exact synchronization capacity, early warnings and accurate recovery; cloud remains OOS, no ranked gap or summary count changes |

| 2026-10-14 | minor | Define bounded entity enablement/edit behavior and actionable atomic failure; no ranked gap or summary count changes |

| 2026-10-14 | minor | Avoid materialization and new catalog/ACK writes for unchanged certified head results; no ranked gap or summary count changes |

| 2026-10-14 | minor | Serialize per-library metadata publication and preserve monotonic completion/attempt-qualified upload states; no ranked gap or summary count changes |

| 2026-10-14 | minor | Serialize per-library metadata publication and preserve monotonic completion/attempt-qualified upload states; no ranked gap or summary count changes |

| 2026-10-14 | minor | Protect writers in both protocols and reserve native sequences outside backups for same-path restores; no ranked gap or summary count changes |

| 2026-10-15 | minor | Retain per-import upload preflight errors and exact retry actions; cloud remains OOS, no ranked gap or summary count changes |
| 2026-10-15 | minor | Resolve shared stack ownership independently of local IDs and local-only overlays; preserve local groups separately; cloud remains OOS, no ranked gap or summary count changes |

| 2026-10-15 | minor | Queue exact local receipt finalization with shared acknowledgements and retry without media retransfers; cloud remains OOS, no ranked gap or summary count changes |

| 2026-10-15 | minor | Report subsystem cloud health, stopped polling, structural conflicts and scan freshness independently of activity history; cloud remains OOS, summary counts unchanged |

| 2026-10-15 | minor | Preserve independent remote metadata/memberships in logical undo and redo; cloud remains OOS, summary counts unchanged |

| 2026-10-15 | minor | Separate transfer/publication/receipt stages; bound fresh media evidence and assert wire costs; cloud remains OOS, summary counts unchanged |

| 2026-10-15 | minor | Share explicit transport outcome variants across native/browser; retain certified unchanged evidence and remove head sentinels from append results; cloud remains OOS, summary counts unchanged |

| 2026-10-15 | minor | LR-CLOUD-SYNC: Extract global structural projection and test convergence separately from local overlay preservation; cloud remains OOS, summary counts unchanged |

| 2026-10-15 | minor | LR-CLOUD-SYNC: Recover native head histories with bounded reference windows and resumable CAS prefix checkpoints; cloud remains OOS, summary counts unchanged |

| 2026-10-15 | minor | LR-CLOUD-SYNC: Bound browser head replay independently of historical payload size; measure native resident catch-up memory and wire costs; cloud remains OOS, summary counts unchanged |

| 2026-10-15 | minor | LR-CLOUD-SYNC: Separate creation from read-only joins and validate append capacity before PUT; cloud remains OOS, summary counts unchanged |

| 2026-10-16 | minor | LR-CLOUD-SYNC: Pin browser export sources through deferred completion and share renderer admission, cancellation and cache-only inspection; cloud remains OOS, summary counts unchanged |

| 2026-10-16 | minor | LR-CLOUD-SYNC: Preserve supporting XMP through managed import and revisions; retain foreign properties and avoid reapplying rendered source recipes; cloud remains OOS, summary counts unchanged |

| 2026-10-16 | minor | LR-CLOUD-SYNC: Retain managed-import request identities and duplicate policy through retry; recover healthy candidates independently with durable errors; cloud remains OOS, summary counts unchanged |

| 2026-10-16 | minor | LR-CLOUD-SYNC: Bind used storage destinations in both protocols before configuration/credential changes; normalize live and proof destinations; cloud remains OOS, summary counts unchanged |


| 2026-10-16 | minor | LR-CLOUD-SYNC: Plan archive proofs incrementally, share node caches and batch missing frontiers before final state merges; cloud remains OOS, summary counts unchanged |

| 2026-10-16 | minor | CR.11: Enforce complete immutable asset manifests and rebind all native/browser blobs, including XMP; cloud remains OOS, summary counts unchanged |

| 2026-10-16 | minor | CR.12: Select shared metadata through explicit schemas and typed reference conversion; retain nonportable smart rules locally; cloud remains OOS, summary counts unchanged |

| 2026-10-16 | minor | CR.13: Extract pure upload readiness, phase, publication and receipt-finalization decisions; cloud remains OOS, summary counts unchanged |

| 2026-10-17 | minor | CR.14: Consolidate current cloud behavior/limits, protocol and dated acceptance; cloud remains OOS, summary counts unchanged |

| 2026-10-17 | minor | CR.15: Resolve live offline owner/quality dependencies after catalog changes and preserve bytes through save/restart; cloud remains OOS, summary counts unchanged |

| 2026-10-17 | minor | CR.16: Adopt existing local records with durable staging and identity/history preservation; cloud remains OOS, counts unchanged |

| 2026-10-17 | minor | CR.17: Browser verified-cache eviction, catalog reservation, live offline selections and temporary viewing; cloud remains OOS, summary counts unchanged |

| 2026-10-17 | minor | CR.18: Retained-deletion notices and reference-checked native orphan reclamation; cloud remains OOS, summary counts unchanged |

| 2026-10-17 | minor | CR.19: Shared native input and pixel admission, explicit browser decode budgets and pinned large 3FR input; cloud remains OOS, summary counts unchanged |

| 2026-10-16 | minor | CUX.10: Include generated demo originals in native cloud mode using full-resolution float DNGs; preserve edits and recovery. Cloud remains OOS; summary counts unchanged |
| 2026-10-16 | minor | CUX.9: Automatically stage existing and future native library photos, reuse verified cloud originals, and remove the duplicate popup settings action. Cloud remains OOS; summary counts unchanged |
| 2026-10-16 | minor | CUX.8: Add guarded desktop whole-library cloud erasure, version/multipart deletion and terminal writer barriers; verify empty-bucket creation. Cloud remains OOS; summary counts unchanged |
| 2026-10-16 | minor | CUX.7: Match corrected cloud popup proportions, semantic status icons, inline upload progress and verified remote original size; cloud remains OOS, summary counts unchanged |
| 2026-10-16 | minor | CUX.6: Redesign the cloud popup around library summary, live transfer progress, grouped attention and contextual actions; cloud remains OOS, summary counts unchanged |
| 2026-10-17 | minor | CUX.11: Resumable native cloud on/off with blocking progress and recovery, verified permanent originals and preserved identities/conflicts. Cloud remains OOS; summary counts unchanged |
| 2026-10-17 | minor | CUX.12: whole-library batch progress, activity-aware remote scans and focused cloud controls; cloud remains OOS, counts unchanged |
| 2026-10-17 | minor | CR.58: Local-only background revisions keep their typed completion context without permitting cloud enrollment. Cloud remains OOS; summary counts unchanged |
