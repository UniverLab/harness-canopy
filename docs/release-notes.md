---
title: Release notes
description: Notable changes to Canopy.
---

# Release notes

## Unreleased

- **Removed identities/seeds/nursery.** The seed identity model, MCP `identity` tool, TUI seed picker, seed prompt injection, and seed transport headers/env vars were removed. On first start after upgrade, an existing `~/.canopy/seeds/` directory is moved once to `~/.canopy/seeds-retired-<YYYY-MM-DD>/` for manual recovery. Sessions that were launched with a seed keep running and resume without seed injection. Identity-related config keys are ignored.
