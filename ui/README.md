# ui

Leptos WASM frontend. Reactive Rust compiled to WebAssembly and served by Trunk.

## Running

Requires `trunk` and the `wasm32-unknown-unknown` target:

```
cargo install trunk
rustup target add wasm32-unknown-unknown
```

Start with the demo server proxied:

```
trunk serve --proxy-backend=http://localhost:3000/
```

Available at `http://localhost:8080`. The `/api/` prefix is stripped by Trunk and forwarded to the demo server on port 3000.

## Pages

- `/` — Landing page. Project overview, pipeline diagram, regime reference, stack.
- `/execute` — Execute page. Submit memo, Jupiter swap, or fault-injected bundle. Live event stream.
- `/log` — Execution log. Table of all submissions with fee, savings, regime, retries, status.
- `/bundle/:id` — Bundle detail. Full lifecycle timeline and AI-generated summary.
- `/network` — Mission Control. Live slots, leader window, tip auction stats, bundle flow.

## Structure

```
src/
  main.rs             WASM entry point
  app.rs              Router, TopNav, global layout
  api.rs              All HTTP and WebSocket client code, shared types
  utils.rs            lamports_to_sol, delta formatting, failure humanization
  pages/
    landing.rs        Home page
    execute.rs        Execute + live event feed
    log.rs            Execution log table
    bundle.rs         Bundle detail + AI summary
    network.rs        Mission Control dashboard
  components/
    result_modal.rs   Inline result card shown after a successful submission
```

## API types

All types that mirror the demo server's JSON responses are in `api.rs`. If the server response shape changes, update the corresponding struct there. The WebSocket event enum `WsEvent` must match the `NetworkEvent` enum in `ingest/src/bus.rs`.

## Design

Dark theme only. Amber accent (`amber-400`, `amber-500`) for primary actions and highlights. Monospace for all data values. All layout via Tailwind CSS utility classes. No icons library — inline Unicode where needed.
