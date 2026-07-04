---
name: Silences
description: Open-source agentic coding framework with DeepSeek-powered chat interface
colors:
  bg-base: "#151517"
  bg-layer-1: "#232324"
  bg-layer-2: "#2C2C2E"
  bg-layer-3: "#353638"
  bg-overlay: "#43454A"
  bg-sidebar: "#1B1B1C"
  brand-primary: "#5686FE"
  brand-hover: "#3964FE"
  brand-text: "#679EFE"
  label-primary: "#F9FAFB"
  label-secondary: "#CFD3D6"
  label-tertiary: "#ADB2B8"
  label-caption: "#81858C"
  label-dimmed: "#43454A"
  bubble-bg: "#2C2C2E"
  input-bg: "#2C2C2E"
  border-l1: "rgba(255,255,255,0.06)"
  border-l2: "rgba(255,255,255,0.12)"
  border-l3: "rgba(255,255,255,0.16)"
  state-error: "#F25A5A"
  state-warn: "#F59E0B"
  state-success: "#22C55E"
typography:
  body:
    fontFamily: "Inter, system-ui, -apple-system, BlinkMacSystemFont, Segoe UI, Roboto, sans-serif"
    fontSize: "14px"
    fontWeight: 400
    lineHeight: 1.5
  label:
    fontFamily: "Inter, system-ui, -apple-system, BlinkMacSystemFont, Segoe UI, Roboto, sans-serif"
    fontSize: "13px"
    fontWeight: 400
    lineHeight: 1.4
  mono:
    fontFamily: "'JetBrains Mono', 'Fira Code', Menlo, Monaco, Consolas, monospace"
    fontSize: "13px"
    fontWeight: 400
    lineHeight: 1.5
rounded:
  sm: "8px"
  md: "12px"
  lg: "16px"
  pill: "9999px"
spacing:
  xs: "4px"
  sm: "8px"
  md: "16px"
  lg: "24px"
  xl: "32px"
components:
  button-icon:
    backgroundColor: transparent
    textColor: "{colors.label-tertiary}"
    rounded: "{rounded.sm}"
    padding: "0"
  button-primary:
    backgroundColor: "{colors.brand-primary}"
    textColor: "#FFFFFF"
    rounded: "34px"
  button-sidebar-new:
    backgroundColor: transparent
    textColor: "{colors.label-tertiary}"
    rounded: "{rounded.sm}"
    border: "1px solid {colors.border-l2}"
  input-chat:
    backgroundColor: "{colors.input-bg}"
    textColor: "{colors.label-primary}"
    rounded: "24px"
    border: "1px solid rgba(255,255,255,0.06)"
  user-bubble:
    backgroundColor: "{colors.bubble-bg}"
    textColor: "{colors.label-primary}"
    rounded: "22px"
  code-block:
    backgroundColor: "{colors.bg-layer-1}"
    textColor: "{colors.label-primary}"
    rounded: "{rounded.md}"
    border: "1px solid {colors.border-l1}"
---

# Design System: Silences

## 1. Overview

**Creative North Star: "The Terminal Workshop"**

Silences is a dark-environment developer tool first — it lives where the user's terminal, editor, and AI agent converge. The visual system is **earned familiarity**: every pixel borrows from the developer tool vernacular (dark surfaces, monospace where it matters, compact information density) but refines it through intentional spacing, a restrained blue accent, and clear information hierarchy. The interface is a workshop bench, not a dashboard or a marketing page. Tools are visible but quiet; the conversation is the center of gravity.

The system explicitly rejects: decorative gradients, glassmorphism, over-rounded elements, side-stripe accents, numbered section markers, and the "hero-metric" SaaS template. It defaults to dark because the primary use case is evening/late-night coding sessions where a bright interface would be fatiguing.

**Key Characteristics:**
- **Dialogue-first.** The chat panel is 70%+ of the viewport. Everything else (sidebar, usage bar, input area) is service furniture around the conversation.
- **Restrained accent.** The blue brand color (`#5686FE`) is reserved for interactive affordances: send button, links, thinking indicator, brand text. It never appears decoratively.
- **Layered darkness.** Depth is communicated through tonal layering (four surface levels: `#151517` → `#1B1B1C` → `#232324` → `#2C2C2E`) rather than shadows or blurs.
- **Developer reading rhythm.** Body chat text at 16px/1.5 for readability, code blocks at 13px/1.5 mono, labels at 12-13px. Line length capped at 65–75ch for prose.

## 2. Colors

### Primary
- **Brand Blue** (`#5686FE` / oklch(57% 0.18 265)): Primary actions, send button fill, brand text, thinking-header accent. Used sparingly — never as a surface tint or decorative stripe.
- **Brand Hover** (`#3964FE` / oklch(50% 0.21 265)): Send button hover state, interactive element hover.

### Neutral
Silences uses four distinct neutral layers to create depth through surface stacking (no shadows on surfaces):

| Token | Value | Role |
|---|---|---|
| `bg-base` | `#151517` | Page background, content area, input section |
| `bg-sidebar` | `#1B1B1C` | Sidebar background |
| `bg-layer-1` | `#232324` | Code block background, avatar area |
| `bg-layer-2` / `bubble-bg` / `input-bg` | `#2C2C2E` | User message bubbles, input container, hover states |
| `bg-layer-3` | `#353638` | Active nav item, citations |

### Text
- **Primary** (`#F9FAFB`): Body content, headings, active labels.
- **Secondary** (`#CFD3D6`): Assistant content, messages.
- **Tertiary** (`#ADB2B8`): Placeholder text, secondary metadata.
- **Caption** (`#81858C`): Timestamps, usage stats, muted metadata.
- **Dimmed** (`#43454A`): Decorative dividers, thought-process borders.

### State
- **Error** (`#F25A5A`): Error messages, destructive actions.
- **Warning** (`#F59E0B`): Warning states.
- **Success** (`#22C55E`): Success indicators, tool result text.

### Named Rules
**The Restrained Accent Rule.** Brand blue appears exclusively on interactive affordances and brand-identity elements. It never fills a surface, never appears as a decorative border stripe, and never exceeds ~5% of any viewport's area.

**The No-Cream Rule.** The background is a true dark neutral (`#151517`) with no warm tint. The dark body is not "warm charcoal" or "cool steel" — it is neutral by default, and any future tint must be toward the brand's own hue (`265°`), not toward generic warmth.

## 3. Typography

**Display/Body Font:** Inter (with system-ui / -apple-system / Segoe UI / PingFang SC / Microsoft YaHei fallback)
**Mono Font:** JetBrains Mono (with Fira Code / Menlo / Consolas / Cascadia Mono fallback)

**Character:** Technical but unhurried. Inter at 14px body provides the clarity developers need for long reading sessions, with Chinese font fallbacks (PingFang SC, Microsoft YaHei) ensuring CJK readability. Mono is reserved for code blocks only — not for labels, buttons, or UI text.

### Hierarchy
- **Body** (400, 16px, 1.5): Chat messages, both user and assistant. The primary reading size. Line length capped at 65–75ch.
- **UI Label** (500, 14px, 1.5): Sidebar titles, top bar labels, button text.
- **Caption** (400, 12–13px, 1.4): Session timestamps, usage statistics, action icon labels.
- **Mobile/Responsive Body** (400, 16px, 1.5): Same as desktop — chat text doesn't scale down.
- **Code** (400, 13px, 1.5): Inline code (`#2C2C2E` background, 6px radius) and code blocks (`#1B1B1C` background, 12px radius).

### Named Rules
**The One-Family Rule.** One typeface (Inter) carries all UI: headings, labels, buttons, body. No pairing, no display face. Mono is a functional face, not a design face.

## 4. Elevation

Silences uses **tonal layering** exclusively. Depth is communicated by stacking surfaces of increasing lightness (`#151517` → `#2C2C2E`), not by drop shadows or blurs. This is consistent with the anti-glassmorphism principle and the terminal-workshop metaphor — real workbenches don't cast shadows on themselves.

Shadows exist only for state feedback:
- **Hover glow** (`0 2px 4px rgba(0,0,0,0.05)`): Subtle lift on the input container when focused.
- **Usage bar** sits flush against the message area with a `1px` border-l2 separator.

### Named Rules
**The Flat-By-Default Rule.** Surfaces are flat at rest. Elevation is communicated exclusively through tonal stacking, not shadows. Shadows appear only as a response to focus state (input focus).

## 5. Components

### Buttons
- **Send Button:** Circular, 34×34px, brand-blue fill. Disabled at 50% opacity, active at 100%. Transitions background 0.2s ease. Hover shifts to brand-hover (`#3964FE`).
- **Icon Button:** 28×28px, 8px radius, transparent background. On hover, `rgba(255,255,255,0.08)` fill appears. Tertiary label color.
- **New Session (+):** 28×28px, 8px radius, 1px border-l2 stroke. Transparent to hover bg transition.

### User Messages
- **Bubble:** 22px radius, `#2C2C2E` fill, max-width calc(100% - 88px). Right-aligned. 10px/16px internal padding. 16px/24px text. No shadow, no border.

### Input Area
- **Container:** 24px radius, `#2C2C2E` fill, 1px near-transparent border (`rgba(255,255,255,0.06)`). Focus state: `0 4px 10px rgba(0,0,0,0.02)` shadow. Internal padding 16px/12px for text, 8px/12px for action bar.
- **Textarea:** Transparent background, inherit font, `#5686FE` caret. Placeholder in caption (`#81858C`).

### Sidebar
- **Header:** 60px tall, border-bottom l2. Title in 14px/500. New-session button right-aligned.
- **Session Item:** 58px min-height, 12px radius, 8px padding. Hover shifts to `#2C2C2E`, active to `#353638`. Avatar 28×28px, 8px radius, layer-1 background.

### Code Blocks
- 12px radius, `#1B1B1C` background, 1px border-l1. 16px internal padding. 13px/22px mono text.

### Thinking / Reasoning
- Left-border accent via a 1px `#43454A` line (no side-stripe border on the container). Header in brand-text (`#679EFE`). Content in tertiary (`#ADB2B8`), 14px/24px.

### Navigations
- **Sidebar session list:** Vertical stack, no dividers between items. Active state is a filled background (`#353638`), not a left-border accent.

## 6. Do's and Don'ts

### Do:
- **Do** use tonal layering (`#151517` → `#2C2C2E`) for depth instead of shadows or blurs.
- **Do** reserve brand blue for interactive affordances and brand identity only.
- **Do** use 22px radius for user message bubbles and 12–16px for cards and containers.
- **Do** keep body text at 4.5:1+ contrast against the dark background.
- **Do** use `text-wrap: balance` on headings, `text-wrap: pretty` on prose.
- **Do** provide skeleton/loading states for all async content.
- **Do** cover default, hover, focus, active, disabled, and loading states on every interactive component.

### Don't:
- **Don't** use `border-left` or `border-right` greater than 1px as a colored accent stripe on cards, items, or callouts.
- **Don't** use gradient text (`background-clip: text` + gradient). Use solid brand blue or label-primary.
- **Don't** use glassmorphism (backdrop-filter blur on surfaces) as default styling.
- **Don't** use the hero-metric template (big number, small label, supporting stats).
- **Don't** use numbered section markers (01 / 02 / 03) as section headers.
- **Don't** use tiny uppercase tracked eyebrows as section kickers.
- **Don't** use `border: 1px solid X` + `box-shadow: 0 Npx Mpx` with M ≥ 16px on the same element.
- **Don't** use border-radius beyond 24px on containers (buttons/tags can pill at 9999px).
- **Don't** use decorative motion, bounce, or elastic easing. Use ease-out-quart/quint/expo for transitions.
- **Don't** use diagonal stripe patterns or repeating-linear-gradient as background decoration.
