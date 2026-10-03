# local-mcp

A **local, single-user MCP server** that gives an LLM tool access to the **live web**
over the **Streamable HTTP** transport. It is meant to run on your own machine
(`http://localhost:PORT`); you decide whether to expose it to a remote model or a
Docker container.

The tools are designed around the fact that a model's knowledge is stale the day it
is written:

- **`current_datetime`** — the real current local date/time (models do not know
  "now"), with a region-aware timezone and a subtle confidence flag.
- **`web_search`** — find current information that is missing or outdated.
- **`fetch_url`** — read a specific page in full (docs, changelogs, release notes),
  with paging and optional link discovery.
- **`explore_site`** — explore *around* a destination: site-restricted search plus
  pages linked from the target and URLs from `robots.txt` / `sitemap.xml`.

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
data or needs a source.

| Argument | Type | Default | Description |
|---|---|---|---|
| `query` | string | — | Search query (required). |
| `max_results` | integer | 8 | 1–20 results. |
| `site` | string | — | Restrict to a domain, e.g. `docs.rs`. |
| `freshness` | enum | `any` | `any` \| `day` \| `week` \| `month` \| `year`. |
| `language` | string | — | Language/region hint, e.g. `en-US`. |

Returns `{ provider, answer?, results: [{ rank, title, url, snippet, published_date? }] }`.
Sponsored DuckDuckGo results are filtered out.

### `fetch_url`

Read a single URL as clean readable text/markdown, with paging.

| Argument | Type | Default | Description |
|---|---|---|---|
| `url` | string | — | Absolute http(s) URL (required). |
| `max_chars` | integer | 20000 | 200–100000 characters. |
| `start_char` | integer | 0 | Offset; continue with `next_start_char`. |
| `include_links` | boolean | false | Also return the page's links. |

Returns `{ url, final_url, title?, content_format, content, truncated, next_start_char?, links? }`.

### `explore_site`

Discover pages around a destination.

| Argument | Type | Default | Description |
|---|---|---|---|
| `target` | string | — | Domain or URL, e.g. `docs.rs/tokio` (required). |
| `query` | string | — | Optional site-restricted search. |
| `max_results` | integer | 20 | 1–50 discovered pages. |
| `include_external` | boolean | false | Include off-domain links. |
| `freshness` | enum | `any` | Recency filter for the search part. |
| `language` | string | — | Language/region hint. |

Returns `{ domain, base_url, search?, pages: [{text, url}], sitemap_urls }`.

---

## Architecture

Dependency direction: `application → domain ← infrastructure`, wired in `main`.

```
src/
  main.rs                     composition root (config → providers → axum + rmcp)
  config.rs                   config.toml + clap overrides
  error.rs                    AppError / ConfigError enums
  domain/                     business logic (no rmcp/axum/reqwest)
    error.rs                  ProviderError + ProviderFuture
    search.rs                 SearchProvider port, WebSearchService, SearchError
    fetch.rs                  ExtractProvider/LinkSource/SiteMapSource ports, FetchService
    explore.rs                ExploreService, target/URL helpers
    clock.rs                  TimeService: system clock + region resolution (jiff)
  application/                MCP adapter / use cases
    server.rs                 ServerHandler + #[tool_router] (thin)
    tools.rs                  argument DTOs, result DTOs, glue to the domain
  infrastructure/             adapters implementing the ports
    http.rs                   reqwest wrapper (timeouts, browser UA, error mapping)
    html.rs                   tl-based DDG parser, readable text, links, sitemap
    providers/                provider factory + concrete adapters
      tavily.rs  duckduckgo.rs  firecrawl.rs  wikipedia.rs  direct.rs
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
| Direct fetch | ✅ | — | extract + links + sitemap | Final fallback; `tl`-based readable extraction. |

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