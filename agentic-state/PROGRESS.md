# Progress log

Newest entries at top. Task ids refer to [tasks.md](./tasks.md).

---

## 2026-10-23 — committed

- `fb04b8f` feat: security, retrieval, resilience and multimodal overhaul (all code, config, deps)
- `155bba1` docs: rewrite tool reference, add security/reliability section, TASK.md §12
- (this file + tasks.md land in the third commit)

Commit split note: the code ships as one commit because the domain/application/
infrastructure changes are type-coupled (new ports/fields/constructors) and any
finer split would leave intermediate commits non-building. The commit message
enumerates each finding addressed for review orientation.

---

## 2026-10-23 — P0+P1+P2 implemented, all green

**Status: 14/14 tasks done.** `cargo build` clean, `cargo clippy --all-targets`
clean, `cargo test` 50/50 passing. Live smoke-tested against the harness's own
acceptance scenario (see scorecard below).

### Commits-by-area (files changed)

- **P0-1 Error sanitizer**: `src/infrastructure/sanitize.rs` (new),
  `src/infrastructure/http.rs` (bodies -> debug logs only, structured
  `status_error`), `src/domain/error.rs` (status/`retry_after_s` on
  Http/Blocked + Display), `src/application/tools.rs`
  (`ToolError::into_error_data` redacts everything).
  Tests: `redacts_signed_oss_style_urls`, `redacts_bearer_and_kv_secrets`,
  `redacts_url_credentials_and_keeps_plain_text`, `keeps_clean_urls_intact`.
- **P0-2 SSRF guard**: `src/domain/fetch.rs` (`validate_fetch_url`,
  `url_host_blocked`, private-IP/domain classifiers incl. CGNAT/link-local/
  IPv4-mapped v6), `src/infrastructure/http.rs` (`redirect::Policy::custom`
  re-checks every redirect), `src/infrastructure/providers/direct.rs` +
  `src/infrastructure/images.rs` (guard at fetch time), config
  `[fetch] allow_private_network`. Tests: `policy_blocks_private_hosts`,
  `policy_rejects_credentials_and_bad_schemes`,
  `blocked_host_never_reaches_extractor`.
- **P0-3 Provenance + cache + stale-if-error**: `src/domain/fetch.rs`
  (LRU/TTL cache keyed on canonical URL, `extractor`/`total_chars`/
  `returned_chars`/`max_chars`/`from_cache`/`cached_age_s`/`stale` fields).
  Tests: `cache_serves_second_fetch_without_extractor`,
  `serves_stale_cache_when_all_extractors_fail`.
- **P1-4 `today` injection**: `src/application/server.rs` + `tools.rs` —
  every `web_search`/`fetch_url`/`explore_site` envelope carries `today`.
- **P1-5 In-document retrieval**: `src/domain/textsearch.rs` (new:
  `find_windows` merged context windows with char offsets, `outline` heading
  tree), args `pattern`/`context`/`outline` in `tools.rs`. Tests:
  `pattern_search_and_paging_offsets`, `outline_returns_heading_offsets`,
  `find_windows_*` (4), `outline_*` (2).
- **P1-6 Backoff + pacing**: `src/infrastructure/http.rs` (`Retry-After`
  parse, 1 jittered retry ≤5s), `src/infrastructure/providers/paced.rs`
  (slot-scheduler per provider, `providers.min_interval_ms=500`).
- **P1-7 GitHub**: `canonicalize_url` blob→raw (texty extensions + known
  names), `parse_github_pull` + `PullRequestSource` port +
  `src/infrastructure/providers/github_api.rs` (cached, optional
  `GITHUB_TOKEN`); boilerplate lines + global long-block dedup in `html.rs`.
  Tests: `github_blob_is_canonicalized_to_raw`, `parses_pull_request_urls`,
  `extract_readable_*`.
- **P1-8 explore_site**: `file_like_note` early bail (host-vs-path aware, so
  `docs.rs` is not a "file"), `include_sitemap=false` default + 25-cap +
  `sitemap_total`. Tests: `file_like_targets_are_detected`.
- **P1-13 HTML→Markdown** (harness idea): `src/infrastructure/html.rs`
  deterministic renderer — `#`..`######`, `- ` lists, ``` fences, `>` quotes,
  `![alt](abs-url)` images; nav/header/footer/form skip; boilerplate filter
  ("Uh oh!…" etc.); dedup. Offsets stable across calls (backed by P0-3).
- **P2-9 Search filters**: `since` (ISO + RFC-2822 aware), `exclude_domains`,
  URL dedup, sentence-boundary `trim_snippet` in `domain/search.rs`; Tavily
  `days_since` mapping. Tests: `post_process_filters_dedups_and_keeps_undated`,
  `since_understands_rfc2822_dates`, `trim_snippet_prefers_sentence_boundary`.
- **P2-10 Cost accounting**: `cost: {chars_returned, est_tokens}` on every
  tool result (added in `to_json_result`).
- **P2-11 Stale-if-error**: folded into the P0-3 cache path.
- **P2-12 Docs**: README tool tables rewritten + new "Security &
  reliability" section; TASK.md §12; `config.toml(.example)` `[fetch]` +
  `min_interval_ms` documented (example/defaults parity test passes).
- **P2-14 Image pass-through** (harness idea): `max_images=0..3` arg; domain
  `ImageSource` port + `markdown_images` content-candidate filter
  (badges/avatars/icons/svg denied); `src/infrastructure/images.rs`
  (same SSRF policy, ≤5 MiB, decode → thumbnail ≤1568 px → JPEG q80 onto
  white); base64 delivered as MCP `ImageContent` blocks alongside the JSON,
  mirrored as metadata in `images: [{url, media_type, bytes}]`.
  New deps: `regex`, `base64`, `image` (feature-trimmed) — justified in
  TASK.md §12. Test: `markdown_images_extracts_content_candidates_only`.

### Live acceptance scorecard (against the harness's own scenario)

| Harness acceptance criterion | Result |
|---|---|
| `search("llama.cpp slot save restore", since=2026-06-01)` with dates | ✅ results carry `published_date` (RFC-2822 parsed for filtering); `today: 2026-10-23` in envelope; SEO-slop excludable via `exclude_domains` |
| `fetch_url(<blob README>, pattern="slot-save-path|cache-reuse")` → 1 call, <3k chars | ✅ 1 call → 2 218 chars, 5 match windows with offsets (was 4 fetches / ~140k chars) |
| `fetch_url(<PR 28092>)` → state in envelope | ✅ `github: {state: "open", merged: false, comments: 9, draft: false}` |
| 0 credentials in any error string | ✅ raw bodies debug-log only; sanitizer tests + live 404/SSRF checks clean |
| 0 provider 429s visible | ✅ retry-then-structured-error + per-provider pacing (no stampede) |
| ≤7 calls | pattern+PR+search+explore ran in 5; typical tasks need strictly fewer than before |

Also live-verified: `from_cache: true` on repeat fetch, `total_chars: 112972`,
outline with correct offsets on the 113k-char README, SSRF refusal message,
explore_site file-bail note, image blocks (2 JPEGs, 21.7k + 4.4k bytes,
valid per `file`).

### Notes for the harness team

- Dropped/mis-ordered parallel results (#3/#4): JSON-RPC ids are correct on
  our side; suspect their transport mux. Repro suggestion unchanged but
  server-side behavior verified per-request.
- `get_datetime` (#12) does not exist in this server — they had two MCP
  servers mounted.
- "Proxy rewriting" (#2): caused by tavily/firecrawl fetching server-side
  (their API design, now documented); use `extract_order=["direct"]` for a
  purely local pipeline. The leaked signed URL in their transcript came from
  an upstream error body — that echo path is now closed.

---

## 2026-10-23 — session start

- Created ledger. Triage of the 13 post-mortem findings recorded in tasks.md
  (confirmed in-repo: #1 error echo, SSRF gap, no backoff, no in-doc search, explore_site
  waste, GitHub handling, filters/accounting; harness-side: correlation/dropped results;
  not present: duplicate datetime tool).
- Starting P0 batch: P0-1 sanitizer → P0-2 SSRF → P0-3 provenance/cache.
