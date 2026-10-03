# TASK: local-mcp

> Status: **implemented and smoke-tested on Linux; provider chains verified live.**
> This file is the single source of truth for scope and decisions.

## 1. Goal

A **local, single-user MCP server** that gives an LLM tool access to the **live web**,
running on the user's own machine and reached over **Streamable HTTP**
(`http://localhost:PORT`). The LLM typically runs on a remote host, so it is the
user's choice whether/how to expose this local server (e.g. via Docker networking).

The server must let the model:

- **search** for information that is missing from its knowledge or likely outdated
  (a model's training data is stale the day it is written),
- **read a specific web page** directly (docs, changelogs, articles) with paging and
  readable-text extraction,
- **explore around a destination** (site-restricted search + linked pages + sitemap).

## 2. Hard requirements

- Rust, using the official MCP crate **`rmcp`** (currently `3.5.0`).
- **HTTP transport** (rmcp Streamable HTTP), not stdio. Bind to localhost by default.
- **Do not use more crates than necessary.**
- **Declare error types ourselves as enums** (no `thiserror`, no `anyhow`).
- **Do not overuse macros.** Use only the macros the crate authors consider
  best-practice (`rmcp`'s `#[tool_router]` / `#[tool]`), plus `serde`/`schemars`
  derives and `clap` derive. No custom macro DSLs, no macro-based orchestration.
- **Layered architecture**: application layer separated from business logic and
  infrastructure. Must be **open for adding more tools/providers** without rewrites.
- Must **compile on Windows and Linux**. Primary target is Linux; keep Windows
  cross-compilation in mind (use `#[cfg]` where genuinely needed and gate any
  platform-specific code behind Cargo features/modules).
- **Logging** via `tracing` + `tracing-subscriber` (mandatory).
- **Unit tests** (mandatory). No integration tests for now.
- Providers must be **free to use (no cost)**. Free accounts are allowed if
  necessary; **paid plans must never be required or supported**.
- **License: Apache-2.0** (public repository); see `LICENSE`.

## 3. Confirmed decisions (from Q&A with the user)

1. "on the client side" means: the MCP server runs **locally** and performs the
   outbound web requests itself; the user decides how to expose it to the remote LLM.
2. Providers: research keyless/free options thoroughly; allow **optional free-account
   logins** (API keys) as a fallback mechanism. Free/private use is the target;
   commercial use is out of scope for now, **but the README must document** which
   providers are restricted to non-commercial use and which must be disabled for
   commercial use.
3. Tool surface: designed from the LLM's perspective (search + direct read + explore
   around a destination) — see section 6.
4. Macros: use only `rmcp`'s best-practice macros; keep orchestration explicit.
5. HTTP client: **`reqwest`**.
6. Transport: plain HTTP on `http://localhost:PORT`, HTTP transport only (Docker /
   same-host / cross-container usage all supported by binding config).
7. Config: **`config.toml` + `clap`**.
8. Logging with `tracing`/`tracing-subscriber`, unit tests, **no** rate limiting and
   **no** response caching, single local user. Focus on Linux; Windows compile
   correctness via `#[cfg]`/features where needed.

## 4. Provider research (probed live, Oct 2026)

Bot protection is the main obstacle. Key findings:

| Provider | Keyless? | Probed result | Verdict |
|---|---|---|---|
| **DuckDuckGo HTML** (`html.duckduckgo.com/html/`) | yes | **GET → HTTP 202 bot challenge.** **POST + `Referer` + browser UA → HTTP 200, 11 real results, snippets, no challenge.** | ✅ primary general web (needs HTML parse; ToS-grey) |
| DuckDuckGo Lite (`lite.duckduckgo.com/lite/`) | yes | POST works, 12 results | ✅ alternative parser target |
| **Tavily keyless** (`POST api.tavily.com/search`, header `X-Tavily-Access-Mode: keyless`) | yes | **HTTP 200, fresh LLM-optimized JSON** (found Rust 1.98.0 release notes from 2026-08-20). Also `/extract` keyless works. | ✅ primary AI search + extraction |
| **Firecrawl keyless** (`api.firecrawl.dev/v1/search`, `/v1/scrape`) | yes | search + scrape HTTP 200 JSON. `/v1/map` → 401 on keyless tier. | ✅ scrape/search; map needs free key |
| Wikipedia Action API | yes | HTTP 200 JSON | ✅ encyclopedic fallback |
| HN Algolia API | yes | HTTP 200 JSON | ✅ optional tech/news |
| Stack Exchange API | yes | HTTP 200 JSON | ✅ optional programming Q&A |
| Marginalia | yes (`public` key) | 429 "Daily Limit Exceeded" / flaky; data license **CC-BY-NC-SA** | ⚠️ optional, **non-commercial only** |
| Bing / Google / Yahoo / Qwant / Yep / Mojeek / Startpage | — | 302/403/captcha/challenge | ❌ do not use |
| Jina Reader (`r.jina.ai`) | no (403) | Cloudflare challenge; free key exists | ⚠️ optional via free key |
| SearXNG public instances | JSON disabled | mostly HTML/429; JSON usually off | ⚠️ supported only via self-hosted instance URL |
| Stract / 4get public API | — | 404 / 401 | ❌ |

**Free-account (API key, no card) options** for the optional login mechanism:
Tavily (1000 credits/mo), Firecrawl (more endpoints/limits), Exa ($20 + $10/mo),
LangSearch (free), Jina (free key).
**Excluded:** Brave Search API (card now required) and any paid tier.

### Default provider strategy

A `SearchProvider` port + ordered, configurable fallback chain.
Default order: `tavily` → `duckduckgo` → `wikipedia`.
Every provider can be enabled/disabled/reordered in `config.toml`;
optional `api_key` fields let free accounts raise limits without code changes.

Content extraction (`fetch_url`) chain: `tavily` extract → `firecrawl` scrape →
direct `reqwest` fetch + `tl`-based readable-text fallback.

## 5. Errors, macros, crates

- Hand-written enums: `ProviderError`, `SearchError`, `FetchError`, `ExploreError`,
  `ConfigError`, `AppError`; each implements `Display` + `std::error::Error`.
  Domain errors are mapped to MCP tool errors in the application layer.
- Dependencies (kept minimal; `serde`/`schemars` are already rmcp transitive deps and
  are declared directly only so derive macros resolve cleanly):
  - `rmcp = { version = "3.5", features = ["transport-streamable-http-server"] }`
  - `axum = "0.8"`, `tokio = "1"` (`rt-multi-thread`, `macros`, `net`, `signal`, `time`)
  - `axum-server = { version = "0.8", optional = true, features = ["tls-rustls"] }`
    (only with the optional `tls` Cargo feature; same rustls stack as reqwest)
  - `tokio-util = { version = "0.7", features = ["rt"] }` (cancellation token for graceful shutdown)
  - `reqwest = { version = "0.13", default-features = false, features = ["rustls", "json", "form", "charset"] }`
  - `tl = "0.7"` (pure-Rust HTML parsing: DDG results + link discovery + readability fallback)
  - `jiff = { version = "0.2", features = ["tzdb-bundle-always"] }`
    (system time zone, IANA zones + DST, bundled time zone data for Windows/Docker)
  - `url = "2"` (URL validation, relative-link resolution, wiki URL encoding)
  - `clap = { version = "4", features = ["derive"] }`
  - `toml = "1"`, `serde = { version = "1", features = ["derive"] }`, `serde_json = "1"`,
    `schemars = "1"`
  - `tracing = "0.1"`, `tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }`

## 6. Tool surface (designed from the LLM's perspective)

Register tools under a `tools/` module so more can be added trivially. Four tools:

### `current_datetime`
Local date/time with region inference and a confidence flag (models do not know "now").
- in: `timezone: Option<String>` (IANA name or `+HH:MM`), `locale: Option<String>`
  (e.g. `de-DE`, `Germany`, `US`)
- out: `{ datetime, date, time, weekday, timezone, utc_offset, unix_seconds, utc,
  timezone_source, certainty, note? }`
- resolution priority: explicit timezone > locale argument > request `Accept-Language`
  > system time zone > configured default (`[time] default_timezone`, default
  `Europe/Berlin`). Missing/conflicting clues -> default zone, `certainty: "low"`,
  short `note`; values stay clean so a wrong region is noticeable but not disruptive.

### `web_search`
Search the public web for current/otherwise-missing information.
- in: `query: string`, `max_results: u8 = 8` (1..20), `site: Option<String>`
  (restrict to a domain), `freshness: enum{any,day,week,month,year} = any`,
  `language: Option<String>`
- out: `{ provider, results: [{rank,title,url,snippet,published_date?}], answer? }`
- Ads (DDG `y.js?ad_…`) are filtered out.

### `fetch_url`
Read a single URL as clean readable text/markdown; supports paging and link discovery.
- in: `url: string`, `max_chars: u32 = 20000` (cap 100k), `start_char: u32 = 0`,
  `include_links: bool = false`
- out: `{ url, final_url, title, content_format, content, truncated, next_start_char?, links? }`
- `links` gives same-page anchors (text + absolute URL) for exploring around a page.

### `explore_site`
Discover pages around a destination.
- in: `target: string` (domain or URL), `query: Option<String>`,
  `max_results: u8 = 20`, `include_external: bool = false`
- behavior: if `query` given → site-restricted search (`site:<domain> <query>`);
  always → fetch target/homepage and extract same-domain links; also read
  `robots.txt`/`sitemap.xml` for URL discovery. Keyless (no dependency on paid APIs).
- out: `{ domain, search_results?, pages: [{url,title?}], sitemap_urls? }`

## 7. Architecture / layout

Dependency direction: `application → domain ← infrastructure`, wired in `main`.

```
src/
  main.rs                     # composition root: config -> providers -> axum + rmcp
  config.rs                   # config.toml + clap overrides
  error.rs                    # AppError / ConfigError enums (hand-written)
  domain/                     # business logic (no rmcp/axum/reqwest)
    mod.rs
    error.rs                  # ProviderError + ProviderFuture (boxed future alias)
    search.rs                 # SearchProvider port, WebSearchService, SearchError
    fetch.rs                  # ExtractProvider/LinkSource/SiteMapSource ports, FetchService
    explore.rs                # ExploreService + target/host helpers
    clock.rs                  # TimeService: system clock + region resolution (jiff)
  application/                # MCP adapter / use cases
    mod.rs
    server.rs                 # ServerHandler + #[tool_router] (thin)
    tools.rs                  # argument DTOs, result DTOs, glue to domain services
  infrastructure/             # adapters implementing the ports
    mod.rs
    http.rs                   # reqwest wrapper: timeouts, browser UA, error mapping
    cors.rs                   # CORS + Private Network Access middleware (hand-written)
    html.rs                   # tl-based DDG parser, readable text, links, sitemap
    providers/
      mod.rs                  # provider factory (honors config order + commercial flag)
      tavily.rs               # search + extract
      duckduckgo.rs           # search (POST + Referer)
      firecrawl.rs            # search + scrape
      wikipedia.rs            # search
      direct.rs               # extract + links + sitemap (implements 3 ports)
```

Adding a tool = new module + one thin handler; adding a provider = new adapter +
one registry entry. `providers/mod.rs` holds the factory/ordering logic.

## 8. Config (config.toml + clap)

- CLI: `--config <path>` (default `./config.toml`), `--bind`, `--log-level`,
  `--providers <csv>` (override order), `--print-example-config`.
- Sections: `[server]` (`bind`, `path`, `json_response`, `legacy_session_mode`),
  `[http]` (`timeout_ms`, `user_agent`), `[providers]` (enabled order +
  optional free-tier `api_key` per provider), `[tools]` (enable/disable),
  `[usage]` (`commercial = false`).
- Environment overrides for keys: `TAVILY_API_KEY`, `FIRECRAWL_API_KEY`, etc.

## 9. Cross-platform

- HTTP server is plain HTTP by default; **optional** direct HTTPS is gated behind the
  `tls` Cargo feature and `[server.tls]` config (off by default, so the reverse
  topology — TLS terminated by a proxy / plain HTTP elsewhere — still works).
- Outbound TLS uses **rustls** (no OpenSSL → Windows-safe); HTTPS serving uses the
  same rustls stack via `axum-server`.
- Browser clients are supported via CORS + Private Network Access response headers
  (`server.cors_origins`).
- Graceful shutdown via `tokio::signal::ctrl_c()` (works on Windows + Linux).
- No unix-only APIs; any platform-specific code isolated behind `#[cfg(...)]`
  and/or a Cargo feature.
- Verify on Linux (`cargo test`, `cargo check`); keep `cargo check --target
  x86_64-pc-windows-msvc` viable.

## 10. README requirements

The README must document:
- Setup (build, `config.toml`, clap usage, Docker/localhost networking, MCP client
  config for Streamable HTTP).
- Tool reference with examples.
- **Free/private vs commercial use**: a table listing every provider, its terms,
  and whether it is allowed in commercial use; config flag `usage.commercial = true`
  disables the non-commercial / ToS-grey providers (at minimum DuckDuckGo scraping
  and Marginalia must be blocked, with attribution requirements for Wikipedia etc.).

## 11. Out of scope (for now)

- Paid providers/plans, API-based auth flows (OAuth).
- Rate limiting, response caching, multi-user/session persistence.
- Integration tests, CI pipeline.

## 12. Verification performed

- `cargo check` and `cargo test` are clean (9 unit tests: HTML/DDG parser, entity and
  percent decoding, links, readable extraction, sitemap, Wikipedia URLs).
- Live smoke test over Streamable HTTP (2025-11-25): `initialize`, `tools/list`, and
  `tools/call` for all three tools.
  - `web_search` via Tavily returned fresh results (Rust 1.98.0 release, 2026-08-20).
  - `web_search` with `--providers duckduckgo` returned 4 live results.
  - `fetch_url` via Tavily extract and via the `direct` extractor (+16 links).
  - `explore_site` on `docs.rs/tokio`: 5 pages + 33 sitemap URLs.
  - `usage.commercial = true` disables DuckDuckGo and logs a warning
    (chain becomes `["tavily", "wikipedia"]`).
  - `current_datetime`: system zone -> `system`/`medium`; `TZ=UTC` + no hints ->
    `Europe/Berlin`, `default`/`low` with a short note; `Accept-Language: en-GB` ->
    `Europe/London`, `request`/`medium`; explicit `timezone: "Asia/Tokyo"` ->
    `explicit`/`high`; conflicting clues -> default zone, `low`, conflict note.
- Browser access: `OPTIONS` preflight returns `204` with CORS headers and
  `Access-Control-Allow-Private-Network: true`; actual responses carry
  `Access-Control-Allow-Origin`.
- Optional TLS: `cargo build --features tls` + `[server.tls]` served HTTPS to a
  self-signed cert (200 `initialize`, live `web_search`, HTTPS CORS/PNA preflight).
  Enabling TLS in config without the feature fails fast with a clear message.