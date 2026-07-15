---
name: Remora
description: A native deep-ocean control surface for coding-agent work.
colors:
  ocean-abyss-dark: "#02082C"
  ocean-surface-dark: "#011B44"
  ocean-raised-dark: "#022753"
  ink-primary-dark: "#EAFBFF"
  ink-secondary-dark: "#A8DCEB"
  ink-muted-dark: "#83AFC2"
  current-cyan: "#0DD5F0"
  current-cyan-strong: "#07F2FB"
  ocean-mist-light: "#F7FCFE"
  ocean-surface-light: "#EAF7FB"
  ocean-raised-light: "#DCEFF5"
  ink-primary-light: "#102A36"
  ink-secondary-light: "#365866"
  ink-muted-light: "#4E6671"
  current-blue-light: "#036F8F"
  success-dark: "#6EA676"
  warning-dark: "#E2A644"
  danger-dark: "#FF5555"
typography:
  title:
    fontFamily: "Berkeley Mono, SFMono-Regular, monospace"
    fontSize: "20sp"
    fontWeight: 600
    lineHeight: 1.2
    letterSpacing: "normal"
  body:
    fontFamily: "Berkeley Mono, SFMono-Regular, monospace"
    fontSize: "14sp"
    fontWeight: 400
    lineHeight: 1.4
    letterSpacing: "normal"
  label:
    fontFamily: "Berkeley Mono, SFMono-Regular, monospace"
    fontSize: "12sp"
    fontWeight: 500
    lineHeight: 1.25
    letterSpacing: "normal"
rounded:
  sm: "8px"
  md: "10px"
  lg: "12px"
  xl: "16px"
spacing:
  xs: "4px"
  sm: "8px"
  md: "12px"
  lg: "16px"
  xl: "24px"
components:
  button-primary-dark:
    backgroundColor: "{colors.current-cyan-strong}"
    textColor: "{colors.ocean-abyss-dark}"
    typography: "{typography.label}"
    rounded: "{rounded.md}"
    padding: "10px 16px"
  button-primary-light:
    backgroundColor: "{colors.current-blue-light}"
    textColor: "#FFFFFF"
    typography: "{typography.label}"
    rounded: "{rounded.md}"
    padding: "10px 16px"
  container-dark:
    backgroundColor: "{colors.ocean-surface-dark}"
    textColor: "{colors.ink-primary-dark}"
    rounded: "{rounded.lg}"
    padding: "12px"
  input-dark:
    backgroundColor: "{colors.ocean-raised-dark}"
    textColor: "{colors.ink-primary-dark}"
    typography: "{typography.body}"
    rounded: "{rounded.md}"
    padding: "10px 12px"
---

# Design System: Remora

## Overview

**Creative North Star: "Deep Current"**

Remora is a restrained native product interface shaped by the deep water in
its canonical icon. Dark mode uses layered navy rather than pure black; light
mode uses cool ocean-tinted neutrals rather than sterile white. Bright cyan is
rare and directional: it marks primary actions, focus, selection, and links.

The interface is dense enough for professional coding-agent work but remains
calm under sustained use. It explicitly rejects the original fork's
black-and-neon-green terminal identity, generic royal-blue AI dashboards, and
decorative glass or gradient treatments. User-selected terminal palettes are
content, not app chrome, and remain visually independent.

**Key Characteristics:**

- Deep navy tonal layering with no decorative shadows.
- One icon-derived cyan action voice.
- Green, amber, and red reserved for semantic state.
- Native controls, compact spacing, and mono-forward typography.
- Cross-platform role parity without pixel-for-pixel imitation.

## Colors

The palette is sampled from the Remora icon and organized by semantic role.
The frontmatter values are normative.

### Primary

- **Current Cyan:** Primary dark-mode actions, links, selection, and focus.
- **Current Blue:** The darker light-mode equivalent, chosen to preserve normal
  text and white-on-button contrast.

### Neutral

- **Ocean Abyss:** Dark base surface.
- **Ocean Surface:** Sidebars, panels, and resting containers.
- **Ocean Rise:** Elevated controls and active chrome.
- **Ocean Mist:** Light-mode base surface with a subtle brand-hue tint.
- **Primary, Secondary, and Muted Ink:** Three explicit content levels for
  hierarchy without opacity-dependent contrast.

### Named Rules

**The One Current Rule.** Cyan owns action and focus and should occupy no more
than roughly ten percent of a task screen. Its rarity gives it authority.

**The Semantic Waterline Rule.** Green always means success or connected,
amber means pending or warning, and red means failure or danger. Brand cyan
must never replace these states.

**The Terminal Isolation Rule.** App chrome follows this system; Ghostty and
other user-selected terminal themes retain their own palettes without tinting.

## Typography

**Display Font:** Berkeley Mono with SF Mono fallback

**Body Font:** Berkeley Mono by default, with SF Pro/system as the user-selected
alternative

**Label/Mono Font:** Berkeley Mono with SF Mono fallback

**Character:** Technical and direct without imitating a novelty terminal.
Native text scaling is part of the type system, not an exception to it.

### Hierarchy

- **Title** (semibold, 20sp, 1.2): Screen and panel titles.
- **Body** (regular, 14sp, 1.4): Messages, settings, and explanations; prose
  should remain within roughly 65–75 characters when layout permits.
- **Label** (medium, 12sp, 1.25): Controls, metadata, status, and compact chips.

### Named Rules

**The Native Scale Rule.** Preserve Dynamic Type and Android font scaling.
Fixed sizes describe defaults only and never cap accessibility scaling.

## Elevation

Depth comes from tonal layering: the raised ocean surface is lighter than the
resting surface in dark mode and darker than the base in light mode. App chrome
does not use decorative drop shadows or glass blur. Modals may use the native
platform scrim, while focus uses a high-contrast cyan outline.

### Named Rules

**The Water-Column Rule.** Elevation is a three-step surface scale, not a shadow
stack: base, surface, raised surface.

## Components

### Buttons

- **Shape:** Gently curved native rectangle (10px reference radius).
- **Primary:** Strong cyan with abyss text in dark mode; Current Blue with white
  text in light mode.
- **Hover / Focus:** Use the defined hover tone and a visible accent focus ring;
  do not add glow or change layout.
- **Secondary / Ghost:** Transparent or surface-backed with primary text and a
  complete hairline border when separation is required.

### Chips

- **Style:** Compact surface or raised-surface fill, 8–10px radius, label type.
- **State:** Selected chips use a full accent outline plus text or icon change;
  color alone is insufficient.

### Cards / Containers

- **Corner Style:** Restrained 8–12px rounding.
- **Background:** Tonal surface roles only.
- **Shadow Strategy:** None at rest; see the Water-Column Rule.
- **Border:** Full-perimeter hairline using the theme border role.
- **Internal Padding:** 12–16px for normal containers, 8px for compact rows.

### Inputs / Fields

- **Style:** Raised tonal fill with a complete hairline border and 10px radius.
- **Focus:** Cyan outline and caret with persistent text contrast.
- **Error / Disabled:** Error pairs red with copy or icon; disabled state uses
  muted ink and reduced interaction, never color alone.

### Navigation

Navigation stays native and compact. The current destination uses cyan plus a
shape, label weight, or icon change. Sidebars and toolbars use the surface and
raised-surface roles rather than transparent glass.

### Pairing and Host Controls

Pairing, server, and harness controls are signature product components. They
must make host identity, transport state, selected runtime, and recovery action
legible without exposing wire-level Alleycat branding as product identity.

## Do's and Don'ts

### Do:

- **Do** derive new brand color from the documented ocean roles.
- **Do** use cyan for primary action, current selection, focus, and links.
- **Do** preserve semantic green, amber, and red with text, icons, or shapes.
- **Do** keep iOS and Android role-equivalent while respecting native controls.
- **Do** preserve user-selected terminal palettes and remote content colors.
- **Do** verify WCAG 2.2 AA contrast in both appearance modes.

### Don't:

- **Don't** restore the original fork's black-and-neon-green terminal identity.
- **Don't** build a generic hacker-terminal surface that uses green for every
  action and state.
- **Don't** use Codex royal blue (`#0169CC`) as Remora's primary identity.
- **Don't** build stock blue SaaS or AI dashboards with ornamental gradients,
  glass cards, or decorative glows.
- **Don't** use side-stripe accents, gradient text, or wide soft card shadows.
- **Don't** recolor Ghostty or another user-selected terminal theme to match app
  chrome.
