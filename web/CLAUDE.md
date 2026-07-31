# Chuggernaut v2 web UI — working notes

The operator UI. React 19 + React Router 7 + Vite + TypeScript. No component library, no
CSS framework.

```sh
npm run dev      # vite dev server (proxies to a local `chuggernaut api` on :8080)
CHUG_API=https://gumbo-mini-0.tail20c474.ts.net npm run dev   # …or against a live deployment
npm run build    # tsc -b && vite build  — run before calling a change done
```

## ⚠️ Always evaluate mobile UI changes on mobile

This app is used on phones. **Any change that touches layout, spacing, widths, tables,
flex/grid, or adds a control MUST be checked at a narrow viewport before you call it done** —
don't reason about it in your head, look at it. In DevTools set the viewport to ~360–390px
wide (or use the device toolbar) and verify:

- **The page never scrolls horizontally.** Wide content (tables, diffs, code, long slugs)
  scrolls *inside its own card* — wrap tables in `<div className="table-scroll">`, and let
  code/diff blocks use `overflow-x: auto`. The `<body>` must not move sideways.
- **Wrapping content never goes in a table cell.** A cell is sized by its content, so a
  wide line (a log, a diff, a long slug) stretches the whole table and its own
  `pre-wrap`/`overflow` never applies — mount such a panel outside the table (#353).
- **Header rows don't collide.** Anything using `.row-head` / `space-between` (title + action
  buttons) should wrap, not overflow. The `@media (max-width: 640px)` block handles the common
  cases — confirm your new element is covered.
- **Controls wrap and stay tappable.** Button groups, tab bars (`.tabs` scrolls horizontally),
  and key/value rows must not push off-screen or shrink below a usable tap target.
- **Inputs stay ≥16px on small screens** so iOS doesn't zoom on focus (the mobile block sets
  this globally — keep it if you restyle inputs).

If you add a new layout pattern, add its mobile rule to the responsive block in `styles.css`
in the same change.

## Styling model

- **One stylesheet: `src/styles.css`.** No CSS-in-JS, no inline style objects for anything
  reusable. All colors come from CSS variables (design tokens at the top); never hard-code a
  hex value in a component.
- **Themes** are `[data-theme='...']` blocks in `styles.css`. To add one: add the block, then
  register its name in `src/theme.tsx`. `index.html` applies the saved theme before first
  paint (mirror the `chug-theme` storage key if you change it).
- The mobile/responsive rules live in the `@media (max-width: 640px)` block near the bottom.

## Data & structure

- **`src/api.ts`** is the single typed wrapper over the HTTP API — add endpoints there, not
  ad-hoc `fetch` calls in components. `ApiError` carries the status.
- **The wire types are generated — do not hand-edit them.** `src/api/types.gen.ts` comes from
  `.chug/schemas/api.schema.json` (which `chuggernaut schema api` derives from the Rust `types`
  crate). After a backend type changes: re-emit the schema, then `npm run codegen`. CI runs
  `npm run codegen:check` for any diff touching `web/` or `.chug/schemas/` and fails on a stale file.
  The shapes that *are* hand-written live in `src/api/envelopes.ts` — the replies the
  dispatcher assembles with `serde_json::json!`, which no Rust type describes. If you need a
  new type there, prefer naming it in Rust and covering it in `cli::schema::api_bundle`.
- `src/api/roundtrip.test.ts` parses Rust-serialized sample payloads against the generated
  types. It runs in CI's web stage; `npm test` runs it locally.
- **`src/useEvents.ts`** handles the SSE event stream (live job/task updates).
- Pages in `src/pages/`, reusable pieces in `src/components/`. Keep components thin and driven
  by `api.ts` types.
