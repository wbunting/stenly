# Stenly marketing site

<!-- impeccable:product-schema 1 -->

## Platform
web

## Stack
User-selected Zola and Tailwind CSS. Code-first approved by user.

## Users
People evaluating an open-source, local-first meeting recorder for macOS and Linux.

## Product Purpose
Explain Stenly and help visitors get the desktop app, not sign into a web application.

## Positioning
Open-source meeting notes with on-device transcription, local storage, and optional self-hosted, OpenAI-compatible summary models. No bot joins the call.

## Capabilities and Constraints
Repository evidence: Parakeet transcription, calendar prompts, microphone and system audio, Markdown/Obsidian notes, templates and recipes, dictation, MCP. macOS 14.2+ (signed, notarized app, v0.1.2); Linux is built from source and has no release yet. Cloud model endpoints and cloud destinations are optional; do not promise data never leaves a device regardless of configuration. The macOS download is live at the latest GitHub release; there is no Linux release, so the site says coming soon. Do not invent successful downloads, hosted availability, customers, or metrics. Do not claim Google Calendar sign-in (not enabled in released builds), automatic updates (the updater is notification-only and not configured in v0.1.2), an in-app note editor or notepad, or verified Google Meet speaker names. Say "no bot", never "invisible" or "secret", and say the user is responsible for recording consent. Keep the hosted destination configurable and omit unavailable hosted actions.

## Brand Commitments
Name: Stenly. Waveform logo on off-white #f4f2e9, with dark ink mark; user prefers this for both website and app over the previous lime tile. Wordmark retains its small olive dot. User requests a fancy magazine-like site, not a Granola clone. Compact single-screen desktop preferred; scrolling on mobile is acceptable. Downloads are the primary action. No web sign-in for this pass. Pricing lives on a separate page: free self-compiled build (MIT, no trial), $10 one-time licence for one Mac (up to 3 activations) with updates included, team self-hosted sharing/deployment by contact. Official app trial before licence: 10 free meetings; only starting new recordings is gated.

## Evidence on Hand
README.md, crates/stenly/assets/logo.svg, and Rust implementation. User-provided editorial typography references (The Shift and bold typographic branding). Synthetic meeting examples must be labeled.

## Product Principles
- Ownership before convenience.
- Explain the product through concrete meeting-note output.
- Be honest about release availability and optional external services.
- Keep the first encounter small and focused.
