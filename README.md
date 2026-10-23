# local-mcp

A **local, single-user MCP server** that gives an LLM tool access to the **live web**
over the **Streamable HTTP** transport. It is meant to run on your own machine
(`http://localhost:PORT`); you decide whether to expose it to a remote model or a
Docker container.

The tools are designed around the fact that a model's knowledge is stale the day it
is written:

- **`current_datetime`** — the real current local date/time (models do not know
  "now"), with a region-aware timezone and a subtle confidence flag. Search and
  fetch responses also carry `today`, so a clock call is rarely needed.
- **`web_search`** — find current information that is missing or outdated, with
  `since`/`exclude_domains` filters and per-call cost accounting.
- **`fetch_url`** — read a specific page as deterministic local markdown with
  stable offsets: paging, in-document `pattern=` grep, `outline=` heading trees,
  GitHub raw/PR-state handling, and optional multimodal image delivery.
- **`explore_site`** — explore *around* a destination: site-restricted search
  plus pages linked from the target (sitemap URLs only on request).

The server is built on the official Rust MCP SDK [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk)
and is deliberately **open for extension**: adding a tool or a search provider is a
small, isolated change (see [Architecture](#architecture)).

---

## Requirements

- Rust (edition 2024; developed with `rustc` 1.98).
- Linux or Windows. No system TLS/OpenSSL is required (rustls).
- Outbound HTTPS access to the providers you enable.

## Build & run

```bash
cargo build --release
./target/release/local-mcp     # creates ./config.toml on first start
```

Optional HTTPS support is compiled in only when requested:

```bash
cargo build --release --features tls
```

Default endpoint: `http://127.0.0.1:8000/mcp`.

Useful flags (all optional, they override `config.toml`):

```bash
local-mcp \
  --config config.toml \
  --bind 127.0.0.1:8000 \
  --providers tavily,duckduckgo,wikipedia \
  --log-level info
```

On first start the server writes a commented `./config.toml` (identical to
`config.toml.example`) if it does not exist yet, so you always have a file to
inspect and edit. `--config <path>` uses a different location and creates the file
there too. `--print-example-config` prints the same documented default to stdout.

---

## Configuration (`config.toml`)

If `./config.toml` (or the path given to `--config`) does not exist, the server
creates it with the documented defaults below on the first start.

```toml
[server]
bind = "127.0.0.1:8000"     # use 0.0.0.0:8000 to expose to a container/another host
path = "/mcp"
json_response = true        # plain JSON replies for simple tools
legacy_session_mode = false # keep false unless a legacy client needs sessions
cors_origins = ["*"]        # browser origins allowed (CORS + Private Network Access)

[server.tls]                # optional direct HTTPS; requires --features tls
enabled = false
cert_path = ""
key_path = ""

[http]
timeout_ms = 15000
user_agent = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/125.0.0.0 Safari/537.36"

[fetch]                     # applied to pages, images, redirects and discovered URLs
allow_private_network = false  # SSRF policy: loopback/RFC1918/CGNAT/link-local/*.internal refused
cache_entries = 16          # in-memory extraction cache (stable paging offsets, free repeat reads)
cache_ttl_secs = 600        # expired entries are still served stale-if-error

[tools]
disabled = []               # e.g. ["explore_site"]

[usage]
commercial = false          # see "Acceptable use" below

[time]
# Timezone used by `current_datetime` when no reliable region clue is available.
default_timezone = "Europe/Berlin"

[providers]
order = ["tavily", "duckduckgo", "wikipedia"]        # search fallback chain (first hit wins)
extract_order = ["tavily", "firecrawl", "direct"]    # content-extraction chain
                                                     # ("tavily"/"firecrawl" fetch server-side; use
                                                     #  ["direct"] for a purely local pipeline)
min_interval_ms = 500       # per-provider pacing: parallel calls queue instead of causing 429s

[providers.tavily]
enabled = true
api_key = ""                # optional free key; falls back to TAVILY_API_KEY

[providers.firecrawl]
enabled = true
api_key = ""                # optional free key; falls back to FIRECRAWL_API_KEY

[providers.duckduckgo]
enabled = true

[providers.wikipedia]
enabled = true
```

### Free-account ("login") mechanism

Every provider works **keyless** by default. If a provider rate-limits you, you can
optionally add a **free-tier** API key (no paid plans are required or supported) via
`api_key` in `config.toml` or an environment variable (`TAVILY_API_KEY`,
`FIRECRAWL_API_KEY`, …). No code changes are needed.

---

## Connecting an MCP client

The server speaks **Streamable HTTP** at `http://localhost:8000/mcp`. A typical
client entry looks like:

```json
{
  "mcpServers": {
    "local-mcp": {
      "type": "http",
      "url": "http://localhost:8000/mcp"
    }
  }
}
```

### Docker / same-host access

The server only ever binds a TCP port; pick the layout that fits:

- **MCP client in a container → server on the host:** bind the server to
  `0.0.0.0:8000` and point the client at `http://host.docker.internal:8000/mcp`
  (or the host's LAN IP). On Linux you may need `--add-host=host.docker.internal:host-gateway`.
- **Server in a container → client on the host:** publish the port
  (`-p 8000:8000`) and connect to `http://localhost:8000/mcp`.
- **Both in containers / both on the host:** use the appropriate service name or
  `localhost`, and set `bind` accordingly.

> Binding to `0.0.0.0` exposes the server to your network. Keep `127.0.0.1` unless you
> intentionally need remote access.

### Browser clients (CORS / mixed content)

If the MCP client runs **inside a browser** (a chat UI served from another origin),
the request is cross-origin and the browser first sends a CORS preflight. When the
page is on a public HTTPS origin and the server is on `127.0.0.1`, Chrome also sends
a **Private Network Access** preflight. The server answers both automatically:

- `cors_origins = ["*"]` (default) allows any browser origin. For tighter security,
  list the exact origins, e.g. `["https://chat.example"]`; an empty list disables CORS.
- Preflights receive `Access-Control-Allow-Origin`, `Access-Control-Allow-Methods`,
  `Access-Control-Allow-Headers`, and `Access-Control-Allow-Private-Network: true`.

**Mixed content:** `http://127.0.0.1` and `http://localhost` are treated as
*potentially trustworthy* by Chrome and Firefox, so an HTTPS page may call the local
HTTP server. If your browser still blocks it, either enable the client's built-in
proxy or serve the MCP server over HTTPS (below).

### HTTPS (optional)

TLS is **opt-in** and never changes the default HTTP behaviour, so topologies where
TLS terminates elsewhere keep working. Build with the `tls` feature, then configure:

```bash
cargo build --release --features tls

# self-signed certificate for local use
openssl req -x509 -newkey rsa:2048 -nodes \
  -keyout key.pem -out cert.pem -days 365 \
  -subj "/CN=localhost" \
  -addext "subjectAltName=DNS:localhost,IP:127.0.0.1"
```

```toml
[server]
bind = "127.0.0.1:8443"

[server.tls]
enabled = true
cert_path = "cert.pem"
key_path = "key.pem"
```

Point the client at `https://localhost:8443/mcp` (trust the certificate or configure
the client to accept it). If `[server.tls] enabled = true` but the binary was built
without the `tls` feature, startup fails with a clear message instead of silently
falling back to HTTP.

---

## Tools

### `current_datetime`

Returns the real current local date/time, weekday, timezone and UTC offset. Call
it whenever an exact date/time matters (scheduling, "today", "latest", age/expiry,
relative time references).

| Argument | Type | Description |
|---|---|---|
| `timezone` | string | Optional IANA name (`Europe/Berlin`) or UTC offset (`+02:00`). |
| `locale` | string | Optional region hint, e.g. `de-DE`, `Germany`, `US`, `New York`. |

Returns `{ datetime, date, time, weekday, timezone, utc_offset, unix_seconds, utc,
 timezone_source, certainty, note? }`.

**How the region is chosen.** Clues are combined in priority order: explicit
`timezone` > `locale` argument > request `Accept-Language` header > the system time
zone > the configured default. If the clues are missing or contradict each other,
the configured default is used and the answer is marked with `certainty: "low"` and
a short `note` (e.g. `timezone assumed: Europe/Berlin (no reliable region hint)`).
The date/time values stay clean — the assumption is only flagged, so a wrong region
is noticeable without corrupting the answer.

### `web_search`

Search the public web. Use it whenever an answer may be newer than your training
data or needs a source. Every response carries `today` (the server's current
date), so time-dependent reasoning never needs a separate clock call.

| Argument | Type | Default | Description |
|---|---|---|---|
| `query` | string | — | Search query (required). |
| `max_results` | integer | 8 | 1–20 results. |
| `site` | string | — | Restrict to a domain, e.g. `docs.rs`. |
| `freshness` | enum | `any` | `any` \| `day` \| `week` \| `month` \| `year`. |
| `since` | string | — | ISO date (`2026-06-01`): drop results published before it (providers with native support get the hint; ISO and RFC-2822 dates are filtered exactly, undated results are kept). |
| `exclude_domains` | string[] | — | Drop results from these domains (host-suffix match; use it against SEO-slop sites). |
| `language` | string | — | Language/region hint, e.g. `en-US`. |

Returns `{ provider, today, answer?, results: [{ rank, title, url, snippet, published_date? }] }`.
Results are deduplicated by URL, snippets over 400 chars are cut at sentence
boundaries, and every response ends with a `cost` object
(`{ chars_returned, est_tokens }`) so the caller can track context budget.
Sponsored DuckDuckGo results are filtered out.

### `fetch_url`

Read a single URL as clean markdown/text. The direct extractor is a
deterministic local HTML→Markdown conversion (headings, lists, code fences,
images as `![]()`), so character offsets are **stable across calls** —
repeat reads are served from an in-memory extraction cache.

| Argument | Type | Default | Description |
|---|---|---|---|
| `url` | string | — | Absolute http(s) URL (required). GitHub `blob/` URLs of text files are automatically fetched from `raw.githubusercontent.com`. |
| `max_chars` | integer | 20000 | 200–100000 characters; the effective value is echoed. |
| `start_char` | integer | 0 | Offset; continue with `next_start_char` or a match's `start_char`. |
| `include_links` | boolean | false | Also return the page's links. |
| `pattern` | string | — | Case-insensitive regex searched **inside** the page: returns matching windows (`matches[].snippet` + `start_char`) instead of the body. Grep a 100k-char doc for ~2k chars. |
| `context` | integer | 160 | Context characters around each `pattern` match (20–2000). |
| `outline` | boolean | false | Return the markdown heading tree with `start_char` offsets instead of the body. |
| `max_images` | integer | 0 | For multimodal clients: download up to 1–3 **content** images (icons/badges/trackers filtered, longest side resized to 1568 px, re-encoded as JPEG) and return them as MCP image content blocks next to the JSON. |

Returns `{ url, final_url, title?, content_format, extractor, today, content,
truncated, next_start_char?, max_chars, returned_chars, total_chars,
from_cache, cached_age_s?, stale, matches?, match_count?, outline?, github?,
images?, links?, cost }`.

Extras:

* **Provenance**: `extractor` says which backend produced the content;
  `total_chars` gives the full document size; `from_cache`/`cached_age_s`/`stale`
  say where the bytes came from. If the whole extractor chain fails but an
  expired cache entry exists, it is served with `stale: true` instead of
  erroring.
* **GitHub pull requests**: fetching a `github.com/<owner>/<repo>/pull/N` URL
  attaches `github: { state, draft, merged, merged_at?, closed_at?, comments }`
  (via `api.github.com`, cached; set `GITHUB_TOKEN` to raise the rate limit).

### `explore_site`

Discover pages around a destination.

| Argument | Type | Default | Description |
|---|---|---|---|
| `target` | string | — | Domain or URL, e.g. `docs.rs/tokio` (required). |
| `query` | string | — | Optional site-restricted search. |
| `max_results` | integer | 20 | 1–50 discovered pages. |
| `include_external` | boolean | false | Include off-domain links. |
| `include_sitemap` | boolean | false | Also fetch the sitemap and return a capped sample (25) plus `sitemap_total`. Off by default — the full dump costs thousands of tokens. |
| `freshness` | enum | `any` | Recency filter for the search part. |
| `language` | string | — | Language/region hint. |

Returns `{ domain, base_url, today, search?, pages: [{text, url}], sitemap_urls,
sitemap_total, note? }`. Targets that look like a single file or item page
(`…/blob/…`, `…/pull/N`, `…/file.html`, …) return a short `note` pointing to
`fetch_url` instead of scraping site chrome around the item.

---

## Security & reliability

* **SSRF policy.** Every URL this server fetches (pages, images, redirects,
  discovered sitemap entries) is refused if its host is loopback, RFC1918,
  CGNAT, link-local, `0.0.0.0`, `*.localhost`, `.local`, `.internal` or an
  IPv6-local address, and URLs with embedded credentials are rejected. Enable
  `[fetch] allow_private_network = true` only when you intentionally want the
  model to reach services on your own machine/network — the fetched content is
  always model-visible.
* **No credential leakage.** Upstream error bodies are never echoed to the
  client: provider failures are reduced to
  `{provider, error_type, HTTP status, retry_after_s}`, raw bodies go to
  `tracing::debug!` logs only, and every outgoing error message is additionally
  scrubbed of signed-URL query strings, `Authorization`/`token`/`key`-style
  secrets and AWS-key-id patterns.
* **Rate-limit friendliness.** One transparent retry with jittered backoff on
  429/5xx (honouring `Retry-After`, capped at 5 s), and a per-provider minimum
  interval (`providers.min_interval_ms`, default 500) that queues parallel
  tool calls instead of letting them stampede an upstream quota. Remaining 429s
  surface as structured `retry_after_s` errors, never raw bodies.
* **Where your data goes.** The `tavily` and `firecrawl` providers fetch the
  target URL on **their** servers — the URL and the page content pass through
  their APIs. For a strictly on-machine pipeline set
  `extract_order = ["direct"]` (the deterministic local extractor) and remove
  `tavily` from the search `order` if you also want search local.
* **Caching.** Successful extractions are kept in memory
  (`[fetch] cache_entries`, `cache_ttl_secs`) so paging offsets stay stable and
  repeat reads cost nothing; on total upstream failure an expired entry is
  served with `stale: true` rather than losing the research thread.

---

## Architecture

Dependency direction: `application → domain ← infrastructure`, wired in `main`.

```
src/
  main.rs                     composition root (config → providers → axum + rmcp)
  config.rs                   config.toml + clap overrides
  error.rs                    AppError / ConfigError enums
  domain/                     business logic (no rmcp/axum/reqwest)
    error.rs                  ProviderError (status/retry_after) + ProviderFuture
    search.rs                 SearchProvider port, WebSearchService, filters/dedup
    fetch.rs                  Extract/Link/SiteMap/PullRequest/ImageSource ports, FetchService
                              (URL policy, canonicalization, LRU cache, stale-if-error)
    textsearch.rs             in-document grep windows + markdown outline (pure)
    explore.rs                ExploreService, target/URL helpers, file-target bail
    clock.rs                  TimeService: system clock + region resolution (jiff)
  application/                MCP adapter / use cases
    server.rs                 ServerHandler + #[tool_router] (thin), cost envelope, image blocks
    tools.rs                  argument DTOs, result DTOs, redaction boundary, glue
  infrastructure/             adapters implementing the ports
    http.rs                   reqwest wrapper (timeouts, browser UA, retry/Retry-After,
                              SSRF redirect policy, no body echoes)
    sanitize.rs               secret/signed-URL redaction for anything model-visible
    html.rs                   tl-based DDG parser, deterministic HTML→Markdown,
                              boilerplate/dedup filtering, links, sitemap
    images.rs                 multimodal pipeline: download, resize, JPEG re-encode
    providers/                provider factory + concrete adapters
      tavily.rs  duckduckgo.rs  firecrawl.rs  wikipedia.rs  direct.rs
      github_api.rs           PR state via api.github.com (cached, GITHUB_TOKEN)
      paced.rs                per-provider pacing queues (min_interval_ms)
```

- **Add a tool:** add a method to `application/server.rs` (delegating into
  `application/tools.rs` → a domain service). No infrastructure changes needed.
- **Add a provider:** implement `SearchProvider` (and/or `ExtractProvider`) in
  `infrastructure/providers/`, then register it in the factory and config order.
- Errors are hand-written enums (`thiserror`/`anyhow` are intentionally not used).
  Only the macros recommended by the `rmcp` authors (`#[tool]`, `#[tool_router]`,
  `#[tool_handler]`) plus standard `serde`/`schemars`/`clap` derives are used.
- Ports return boxed futures so `dyn SearchProvider` works without `async_trait`.

## Providers

Providers are a configurable fallback chain. Default search chain:
`tavily → duckduckgo → wikipedia`. Default extraction chain:
`tavily → firecrawl → direct`.

| Provider | Keyless | Optional free account | Used for | Notes |
|---|---|---|---|---|
| **Tavily** | ✅ (`X-Tavily-Access-Mode: keyless`) | free key, 1000 credits/mo, no card | search + extract | LLM-optimized, freshest results; rate-limited keyless. |
| **DuckDuckGo** (HTML) | ✅ (POST + `Referer` + browser UA) | — | search | Broad web index. May return a bot challenge; then the chain falls through. |
| **Wikipedia** | ✅ | — | search | Encyclopedic only. |
| **Firecrawl** | ✅ for `/search` & `/scrape` | free key for `/map` & higher limits | search + extract | `/v1/map` needs a key (not used; `explore_site` uses direct discovery instead). |
| Direct fetch | ✅ | — | extract + links + sitemap | Final fallback; deterministic local HTML→Markdown (offline, offsets stable, nothing leaves the machine). |

> DuckDuckGo's keyless HTML endpoint is unofficial: it is reached with a browser-like
> `POST`, and it can still serve an HTTP 202 challenge. The fallback chain handles
> that automatically.

## Acceptable use: free/private vs commercial

This project targets **free, private, single-user** use. Some providers are fine
commercially, others are not. Set `usage.commercial = true` when using it in a
commercial context; this **automatically disables** the providers marked
*blocked for commercial use* below and logs a warning.

| Provider | Free / private use | Commercial use | Action when `usage.commercial = true` |
|---|---|---|---|
| Tavily | ✅ keyless or free key | ⚠️ review Tavily's terms; use your own account/plan | stays enabled |
| DuckDuckGo HTML | ✅ (unofficial) | ❌ **not permitted** (automated scraping, no license) | **disabled** |
| Wikipedia | ✅ | ✅ with attribution (content is CC BY-SA) | stays enabled |
| Firecrawl | ✅ keyless/free tier | ⚠️ review Firecrawl's terms | stays enabled |
| Marginalia * | ✅ | ❌ result data is **CC-BY-NC-SA** (non-commercial) | not enabled by default; do not enable commercially |

`*` Marginalia is documented as an example of a non-commercial provider; it is not
implemented in this version but the `SearchProvider` port makes it a small addition.

Also note:
- Web content returned by the tools remains subject to each site's own license and
  terms; cite sources and respect `robots.txt` and copyright.
- Keyless endpoints are shared and rate-limited. For heavier or commercial usage,
  configure your own free/paid provider account and comply with its terms.
- Never commit `config.toml` containing API keys (it is `.gitignore`d).

## Cross-platform

- Pure-Rust TLS (rustls) for outbound requests; direct HTTPS serving (optional) uses
  the same rustls stack via `axum-server`.
- No unix-only APIs; graceful shutdown uses `tokio::signal::ctrl_c()`.
- Verified on Linux; keep `cargo check --target x86_64-pc-windows-msvc` green.

## Testing

```bash
cargo test     # unit tests for the HTML parser, URL handling, config, etc.
cargo check
```

## License

Licensed under the **Apache License, Version 2.0**. See [`LICENSE`](LICENSE).
Provider data and services remain under their own terms — see
[Acceptable use](#acceptable-use-freeprivate-vs-commercial).