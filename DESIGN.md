# Design System — sandwich-rs

Authoritative source for all visual and UI decisions. Read this before writing any frontend code.

## Product Context
- **What this is:** Real-time Solana MEV sandwich detector with public read-only dashboard
- **Who it's for:** Crypto-quant Twitter, MEV researchers, hiring managers at Jump/Wintermute/Cumberland-tier firms
- **Space/industry:** Crypto / DeFi / on-chain analytics
- **Project type:** Single-page web app (read-only), desktop-first

## Direction LOCKED
Mirror Claude.ai's old UI aesthetic — two-column dark layout, coral/orange accent in the spirit of Anthropic's mark, serif hero, generous whitespace, table-as-content (not card grid). Meditative + dense, like reading a financial paper at 2am. Operator-locked 2026-04-28.

## Aesthetic Direction
- **Direction:** Calm utilitarian — restrained, mono-forward, serif-touched. Editorial dignity over Bloomberg neon
- **Decoration level:** Minimal — typography and a single accent color do all the work
- **Mood:** Quiet authority. The product feels like it knows something you don't, and trusts you to read carefully
- **Reference vibes:** Linear (restraint), Stripe (typography precision), Claude.ai (the explicit mirror), Vercel marketing
- **Anti-vibes:** Web3 neon, animated gradients, glassmorphism, SaaS card chrome, AI-slop shadcn defaults

## Typography
- **Display/Hero serif:** `Tiempos Headline, Charter, ui-serif, Georgia, serif` (hero number only)
- **UI sans:** `Inter, ui-sans-serif, system-ui, sans-serif`
- **Mono:** `JetBrains Mono, ui-monospace, Menlo, monospace` — every hash, address, amount, timestamp, pool name
- **Loading strategy:** Self-host Inter + JetBrains Mono via `@fontsource`; Tiempos has no free license — fall back to Charter (system on Windows/Mac modern) and ship `Cooper Hewitt`/`Newsreader` web font as a near-match if Charter unavailable
- **Scale:**
  - Hero number: `64px / 1.05 / weight 400 / letter-spacing -0.025em / tabular-nums` (serif)
  - H1 page title: `32px / 1.2 / weight 400` (serif)
  - H2 section: `18px / 1.3 / weight 500` (sans)
  - Body: `14px / 1.5 / weight 400` (sans)
  - Table-data: `13px / 1.4 / weight 400 / tabular-nums` (mono)
  - Table-header: `11px / 1.2 / weight 500 / uppercase / letter-spacing 0.08em` (sans, color `fg-dim`)
  - Caption / nav: `13px / 1.4 / weight 500` (sans)

## Color
Dark only, **warm dark** (faithful to Claude.ai's UI). Slight sepia/olive undertone — never neutral grey-black. No light mode in v1.

| Token | Hex | Use |
|---|---|---|
| `bg-main` | `#262624` | Main canvas — warm dark |
| `bg-sidebar` | `#1F1E1D` | Sidebar (slightly darker, same warm family) |
| `bg-card` | `#30302E` | Hover row, elevated panels |
| `bg-input` | `#2A2A28` | Input fields, code blocks |
| `border` | `#3A3835` | Hairline dividers, table row separators |
| `border-subtle` | `#33312E` | Sidebar / main column divider |
| `fg` | `#F5F4ED` | Primary text, hero number — warm off-white |
| `fg-muted` | `#B5B2A7` | Secondary text, subtitles |
| `fg-dim` | `#6E6B61` | Table headers, deemphasized labels |
| `accent` | `#D97757` | Brand mark, active nav, "↗" trend arrow, coral the Anthropic family |
| `accent-hot` | `#E89070` | Detection cinematic peak (1.2s decay) |
| `success` | `#7DAA8A` | Attacker profit (muted sage, NOT crypto-green) |
| `danger` | `#C66B5C` | Victim loss (rust, coral cousin — NOT alarm-red) |

**Why warm:** Claude's UI is not neutral. It has a sepia/olive undertone that makes long reading sessions easier on the eyes vs a cold #1A1A1A grey-black. The whole product feels like a desk lamp, not a server rack.

**Restraint rule:** the hero dollar amount is `fg`, NEVER `accent`. Coral is the accent system; the headline lives in warm white. Borrowing Anthropic's discipline.

## Layout

### Two-column, the Claude mirror
```
┌──────────┬────────────────────────────────────────────────┐
│          │              [top-right status]                │
│ sidebar  ├────────────────────────────────────────────────┤
│  260px   │                                                │
│          │              ●  sandwich.rs                    │
│  brand   │                                                │
│  nav     │      $1,247,392 extracted · last 24h           │
│          │      1,847 sandwiches · ↗ +18.2%               │
│          │                                                │
│          ├────────────────────────────────────────────────┤
│          │   TIME    POOL   VICTIM   PROFIT   ATTACK   TX │
│          │   ────────────────────────────────────────     │
│          │   00:00:01 RAY-SOL  $1,238  $4,127  4xZ...  ↗  │
│          │   ...                                          │
│          ├────────────────────────────────────────────────┤
│          │              Top attackers · 24h               │
│          │              [leaderboard table]                │
│          │                                                │
│  footer  │                                                │
└──────────┴────────────────────────────────────────────────┘
```

- **Sidebar:** 260px fixed. `bg-sidebar`. Padding `20px 16px`. Brand at top (mark + wordmark). Nav: `Live feed` (active, `/`), `Leaderboard` (`/leaderboard`), `Methodology` (`/methodology`). Footer: mainnet RPC status indicator (small dot, color = WS health) + GitHub link. **No** starred section. **No** recent sandwiches list (the main feed IS this). **No** account/free plan footer.
- **Main:** flex 1, max-width content `1080px` centered, padding `40px 48px`. Top-right shows `Mainnet · 24h: $X` in 12px mono `fg-dim`.
- **Mobile (<768px):** stacked. Hero + last 5 rows, no sidebar. v1 is a desktop flex piece — not optimized for phone.

### Active nav state
- 2px coral left bar (`accent`) flush to sidebar edge
- Background `bg-card`
- Foreground `fg` (full white-ish)
- Hover (non-active): bg `#1D1D1D`, no bar, fg slight brighten

## Components

### `<Sidebar>`
Brand at top, vertical nav, footer status. Always-mounted across pages.

### `<HeroStatBand>` (Live feed page only)
Center column, 160px tall. Mark + wordmark inline at top (mark 8px left of wordmark, optically aligned to cap height). Below: hero number serif 64px in `fg`. Subtitle 14px sans `fg-muted` with `↗ +18.2%` token in `accent`. **No chart.** Number IS the chart.

### `<FeedTable>`
Sticky header. 44px row height. Columns and widths:

| Column | Width | Format | Align |
|---|---|---|---|
| time | 10% | `00:00:01` mono | left |
| pool | 18% | `RAY-SOL/USDC` mono | left |
| victim loss | 14% | `$1,238.42` mono tabular | right |
| attacker profit | 14% | `$4,127.18` mono tabular | right |
| attacker | 22% | `4xZ...kP9` mono truncated | left |
| tx | 22% | `↗` icon + last 8 of sig mono | left |

Hover row: `bg-card`. Click attacker → `/leaderboard?attacker=X`. Click tx → opens Solscan in new tab. Cap visible at 50; older fade `opacity 0.3` over the last 5, drop after.

### `<LeaderboardTable>`
Same row mechanics. Columns: rank · attacker · 24h sandwiches · 24h profit · top victim pool. Hero column: 24h profit, mono, success-tinted.

### `<MethodologySection>` (`/methodology`)
Single scroll, max-width `720px`, comfortable reading width. H1 "How sandwich.rs detects MEV". Three H2 sections:
1. **Detection Algorithm** — pseudocode block + ASCII flow diagram
2. **Precision Estimate** — one big serif number (e.g., `94.2%`) with subtitle `n=83 labeled samples · vs Sandwiched.wtf`
3. **What we exclude** — false positive cuts (self-trades, MM rebalancing, accidental pool drift)

Code blocks: `JetBrains Mono 13px / bg #0F0F0F / 1px border #2A2A2A / 8px radius / 16px padding`.

### `<EmptyState>`
"Watching mainnet... no sandwiches detected in the last 60s." 14px `fg-muted`, mono pulsating dot before text. NEVER show sad-cloud SVG or any illustration.

### `<ConnectionStatusIndicator>` (sidebar footer)
6px dot. Colors: `success` (WS healthy <30s since last frame), `accent` (degraded, between 30s and 2min), `danger` (disconnected >2min). Tooltip on hover shows last frame time.

## The mark
```
████████  ████████      ← top slab (16×3, radius 1)
   ──── ────             ← filling line (12×1)
████████  ████████      ← bottom slab (16×3, radius 1)
```
16×16 SVG. Solid `accent` (`#D97757`). Scales to 12×12 in nav, 24×24 in hero, 32×32 favicon. Two horizontal slabs flanking a thin filling line. Reads as sandwich at favicon size, abstract enough not to look like clipart. Same color value as Anthropic's mark by *family* (warm coral) — not a copy.

## Motion

### Detection cinematic (calmer, not Bloomberg flashy)
New row inserts at top of feed table over **1.2s ease-out**:
- Row background: starts `#3A2A22` (coral-tinted warm dark), interpolates to `bg-main` `#262624`
- 1px coral left border on the row, fades over the same 1.2s
- **No** glow, **no** flash burst, **no** sound, **no** toast, **no** "+1 NEW" badge
- Existing rows shift down via `transform: translateY` (no layout reflow jank)

### Connection-state transitions
Indicator dot color: 200ms ease-out cross-fade. No pulsing in healthy state — pulse only when degraded.

### Page transitions
None. Hard cut between `/`, `/leaderboard`, `/methodology`. The site doesn't dance.

### Hover micro
Nav link: 100ms ease bg shift. Table row: instant bg shift on hover (responsiveness > polish).

## Spacing
- **Base unit:** `4px`
- **Density:** Comfortable (not compact, not airy)
- **Scale:** `2xs(2) xs(4) sm(8) md(12) lg(16) xl(24) 2xl(32) 3xl(48) 4xl(64) 5xl(96)`
- **Component padding:** sidebar `20px 16px`, main `40px 48px`, table-cell `12px 16px`, card `24px`
- **Border radius:** scale `sm(4) md(6) lg(8) full(9999)`. Buttons + cards `md`. Indicator dot + mark `full`. Tables `0` (sharp).

## Accessibility
- All text on `bg-main` (`#262624`) hits WCAG AA: `fg` (`#F5F4ED` on `#262624` ≈ 13.1:1, AAA), `fg-muted` (`#B5B2A7` on `#262624` ≈ 7.4:1, AAA), `fg-dim` (`#6E6B61` on `#262624` ≈ 3.0:1 — restrict to ≥14px secondary metadata only, never required information)
- Accent on bg: `#D97757` on `#262624` ≈ 4.4:1, AA pass for normal text; AAA fails — use accent for accents only, never body
- Keyboard: full tab traversal in source order. `<a>` for links (Solscan, GitHub). `<button>` for actions. Focus ring: 2px `accent` outline + 2px offset
- Screen readers: feed table updates announced via `aria-live="polite"` on the `<tbody>`. New row gets `aria-label="Sandwich detected at TIME, attacker extracted PROFIT from victim pool POOL"`
- Reduced motion: `@media (prefers-reduced-motion: reduce)` disables the row insert animation; new rows just appear

## Don't (anti-patterns to flag in /qa-design-review)
- Web3 neon, animated gradients, glassmorphism
- Crypto purple-pink-cyan (#9945FF, etc.) — accent stays coral, brief Anthropic family
- Card grids of 3 columns of icons — we use tables
- Centered everything — the page is left-anchored after the hero band
- Decorative rounded-pill chips for nothing
- Hero number in coral (it's `fg`)
- Pulsing healthy state indicator (only degraded states animate)
- Sad-cloud empty states or any illustration
- Toasts, modals, "view detail" pages — every row links to Solscan, that IS the detail view

## README hero screenshot
1600 × 900px. PNG. Frame:
- Sidebar visible full height (left 260px)
- Hero stat band centered in main column, 0–225px vertical
- Feed table 225–900px, exactly 14 rows visible, top row mid-detection (coral border still partially visible at ~400ms into animation)
- Pure black background bleeds to screenshot edge — no browser chrome, no window frame
- Cropped, not screenshotted-then-cropped — render at exactly this resolution

## Decisions Log
| Date | Decision | Rationale |
|---|---|---|
| 2026-04-28 | Initial design system locked | Operator chose Claude UI mirror direction. Codex outside-voice refined to specific hex/type/spacing. |
| 2026-04-28 | Dark-only, no light mode | Codex: "v1 is dark only. Don't ship it." Operator agreed. |
| 2026-04-28 | Hero number is `fg` not `accent` | Restraint discipline. Coral is for accents. Headlines stay neutral. |
| 2026-04-28 | Mobile = hero + last 5 only | v1 is desktop flex piece. Phone is afterthought, not target. |
| 2026-04-28 | Sandwich mark = two slabs + filling | Reads as sandwich at favicon size, abstract enough to be a logo. |
| 2026-04-28 | Palette WARMED to match Claude UI | Operator confirmed the screenshot is the source of truth. Shifted from neutral `#1A1A1A` to warm `#262624` family. Sepia/olive undertone, not grey-black. |
| 2026-04-29 | LAYOUT PIVOT: top-nav, no sidebar | Operator clarified: Claude screenshot was a *palette* reference, layout was open. Three nav items (Live feed, Leaderboard, Methodology) don't justify a 260px left rail. Switched to a sticky 56px top nav with brand left, links inline, connection status + github right. Single centered column max-width 1080px. Hero number bumped 64px → 88px. Subtitle now serif italic "extracted by MEV searchers · last 24h" reading as a sentence into the dollar amount. Sidebar.tsx deleted. |
