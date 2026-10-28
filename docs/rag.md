---
title: RAG Pipeline
description: Local-first semantic search over your own documents.
order: 16
---

# RAG Pipeline

Canopy includes a personal RAG (retrieval-augmented generation) pipeline:
index your own documents, search them semantically, and inject results
into agent sessions.

## Capabilities

- **Semantic search** — embed and query Markdown, MDX and PDF documents.
- **Local embeddings** — local ONNX models (fastembed/BGE, multilingual-e5).
  No cloud provider, no API key.
- **Language-aware chunking** — Markdown split by headings with paragraph
  fallback, similarity-aware merging, and overlap for context
  preservation.
- **Robust PDF extraction** — runs in an isolated subprocess so a parser
  crash can't take down the daemon; HTML detection and raw-text salvage
  fallback.
- **Vector store** — embeddings persisted in LanceDB under
  `~/.canopy/rag/vectors.lancedb`.

## Auto-ingestion

A background watcher monitors your configured RAG roots with a 3-second
debounce, enqueues changes, and reconciles orphan chunks on startup. A
maintenance pass runs at startup and every 6 hours: it compacts the store,
re-runs both reconciliations, and computes a health status.

```bash
canopy rag auto-index start   # resume automatic indexing (default)
canopy rag auto-index stop    # pause without losing the queue
canopy rag report             # per-file report + store footprint and health
canopy rag compact            # merge fragments and prune old store versions
```

## Store maintenance

The vector store maintains itself — no command is required:

- Chunks go in one batch per file, and the store is compacted when an
  indexing drain ends, every 500 inserted chunks, and on the 6-hourly tick.
- `canopy rag report` prints the store's fragments, versions, bytes on disk,
  and the latest health status with its reasons.
- `canopy doctor` prints the same health, one line per reason, with the fix
  command for each.
- `canopy rag compact` compacts immediately: it asks a running daemon to do
  it (its own store handle does the work), or opens the store itself when
  the daemon is down.

Maintenance only ever compacts, purges orphan chunks and reconciles the
ledger against the store. It never resumes paused indexing, never purges
the store, and never downloads a model.

## Exclusions

Create a `.canopy/ragignore` file with regex patterns to exclude files
and directories from indexing.

## Searching

Agents call the `rag` MCP tool's `search` action (rate-limited to 10 calls/minute
per agent with a sliding window). From the TUI, the **RAG transfer
modal** sends search results to any agent as injected context.

RAG roots and embedding settings are configured during `canopy setup`.
