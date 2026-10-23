# Task ledger — harness feedback implementation

Source: harness post-mortem (2026-10 research session). Triaged against codebase on 2026-10-23.
Progress log: [PROGRESS.md](./PROGRESS.md)

Status legend: `[ ]` todo · `[~]` in progress · `[x]` done · `[!]` blocked/wontfix

## P0 — security & trust (first)

- [x] **P0-1 Error sanitizer** — never echo upstream error bodies verbatim. Emit
      `{provider, error_type, http_status, retry_after_s}`; redact URLs, query strings and
      secret-shaped tokens (`Signature=`, `AccessKeyId=`, `token=`, `key=`, `Bearer …`) from
      every string that can reach the model. Raw body only into `tracing::debug!`.
      Files: `src/infrastructure/http.rs`, `src/domain/error.rs`, `src/application/tools.rs`.
      Fixes post-mortem #1 (leaked signed OSS URL).
- [x] **P0-2 SSRF guard** — `validate_url` currently only checks `http(s)://`. Block loopback,
      RFC1918, CGNAT 100.64/10, link-local, `::`, `*.localhost`, `.local`, `.internal`,
      `.home.arpa`; reject credentials in URLs; enforce same policy on redirects via
      `reqwest::redirect::Policy::custom`. Config `[fetch] allow_private_network = false`.
      Files: `src/domain/fetch.rs`, `src/infrastructure/http.rs`, `src/config.rs`, `src/main.rs`.
      Addresses post-mortem #2 (SSRF half; the "proxy rewrite" half is not this repo — provider
      APIs fetch server-side by design; will document).
- [x] **P0-3 Fetch provenance + extraction cache** — add `extractor` (provider name),
      effective `max_chars`, `returned_chars`, `total_chars`, `from_cache`/`cached_age_s` to
      `fetch_url` output. Cache successful extractions (small LRU + TTL) inside `FetchService`
      so `start_char` paging is stable across calls and repeat reads are free.
      Fixes ambiguity of post-mortem #8; foundation for P2-10/11.

## P1 — correctness & retrieval power

- [x] **P1-4 Inject `today`** — add `today` (and `now_utc`) to `web_search`, `fetch_url`,
      `explore_site` envelopes using the existing `TimeService`; model never needs a separate
      clock call. Mitigates post-mortem #3/#6.
- [x] **P1-5 `fetch_url(pattern, context, outline)`** — regex grep with context windows over
      the extracted doc (match offsets feed `start_char` paging) + markdown heading outline with
      char offsets. Post-mortem #6, the biggest token sink.
- [x] **P1-6 Backoff + Retry-After + provider pacing** — capture `Retry-After` on 429/5xx,
      surface `retry_after_s`, one transparent jittered retry; per-provider minimum-interval
      queue so parallel harness calls serialize per provider. Post-mortem #5/#16.
- [x] **P1-7 GitHub handling** — canonicalize `github.com/…/blob/…` → `raw.githubusercontent.com`
      (texty extensions only); enrich `github.com/<o>/<r>/pull/N` fetches with
      `{state, merged, merged_at, closed_at, comments}` via `api.github.com` (cached, injectable
      port); boilerplate stripper for direct-extracted text ("Uh oh!…" nodes, repeated chrome,
      duplicate long paragraphs). Post-mortem #7/#9.
- [x] **P1-8 `explore_site` file bail + sitemap gating** — detect file-like targets (`.md`,
      `/blob/`, `/pull/`, …) and return a short note pointing to `fetch_url` instead of scraping
      GitHub chrome; `sitemap_urls` behind `include_sitemap=false`, else return
      `sitemap_count` + small sample. Post-mortem #7.
- [x] **P1-13 Deterministic HTML → Markdown extractor** (harness idea 2026-10-23) —
      `extract_readable` emits structural markdown (headings → `#`, lists → `-`, code → fences,
      blockquotes) instead of flat text; boilerplate/dup filtering in the same pass.
      Benefits: provider-independent & offline, stable offsets (paging/grep/outline consistent
      across calls), makes `outline=` work for `direct` extraction, cheaper than API scrapers,
      content never leaves the machine. Direct-extracted pages get `format: markdown`.

## P2 — search quality & accounting

- [x] **P2-9 Search filters** — `exclude_domains` (host-suffix post-filter), `since` (ISO date →
      provider-native windows where possible), URL dedup, sentence-boundary snippet trimming,
      `exclude_domains` in args + domain filter in `domain/search.rs`. Post-mortem #11/#19.
- [x] **P2-10 Cost accounting** — `chars_returned` + `est_tokens` on every tool output.
      Post-mortem #13/#20.
- [x] **P2-11 Stale-if-error** — when the whole extractor chain fails, serve expired cache
      entry with `stale: true` instead of an error. Builds on P0-3 cache.
- [x] **P2-12 Docs** — README/TASK.md/config.toml.example: new params, SSRF policy,
      "tavily/firecrawl fetch server-side" privacy note + how to set `extract_order=["direct"]`
      for sensitive setups.
- [x] **P2-14 Image pass-through for multimodal models** (harness idea 2026-10-23) —
      `fetch_url(..., max_images=N)` returns up to N *content* images (article-area
      heuristics: `<img>` inside main blocks, alt-text present, byte/dimension floors to drop
      trackers/icons) as MCP `ImageContent` blocks alongside the JSON text block. Pipeline:
      download (same SSRF policy + size cap ~2 MiB) → decode via pure-Rust `image` crate →
      resize longest side ≤ 1568 px → JPEG/PNG base64. Design constraints: images are opt-in
      (token-expensive, most are decorative), counted into the per-call cost accounting, and
      per-page cap ≤ 3. Depends on P0-3 cache (key by src URL).

## Explicitly out of scope (reported back to harness authors)

- [!] call↔result correlation (#4): JSON-RPC already carries ids; ordering/dropping is the
      harness transport layer, not this server.
- [!] Rotating the leaked Aliyun AccessKey: it belongs to an upstream provider's signed URLs —
      notify the vendor; our side only needs to stop echoing (P0-1).
- [!] Duplicate `get_datetime` tool (#12): does not exist in this repo (only
      `current_datetime`); harness likely had two MCP servers mounted.

## Definition of done per task

1. `cargo build` clean, `cargo clippy` clean, `cargo test` green (new unit tests for pure logic).
2. Progress entry appended to PROGRESS.md with file list + test names.
3. Task box checked here.

## Follow-ups consciously deferred

- Per-**turn** running token total: stateless server can't own this; the
  per-call `cost` object lets the harness accumulate.
- Content-addressed corpus (`ref=sha256…`): the P0-3 cache already removes the
  repeat-fetch cost that motivated it; revisit if the harness wants refs.
- Authority/`is_primary_source` ranking: provider-side signal we don't have;
  `exclude_domains` is the deterministic workaround for now.
- `/releases` → `/releases.atom` canonicalization: changes content shape;
  skipped to avoid surprising output (the `raw` blob rewrite had clear value).
- DuckDuckGo `df=` freshness pass-through: DDG's HTML endpoint accepts
  d/w/m but is our 2nd/3rd provider; freshness already maps on Tavily.
- Aggressive GitHub-chrome classification beyond header/nav/footer skip +
  boilerplate lines + global block dedup.
