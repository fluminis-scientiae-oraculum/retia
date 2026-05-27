# retia-wasm

WebAssembly build of **retia**, a Rust-only fork of [CozoDB](https://github.com/cozodb/cozo). For native code in your application, prefer [`retia`](../retia-core) directly — WASM is slower than native.

See the [main repository README](https://github.com/fluminis-scientiae-oraculum/retia) for project overview.

## Building

```bash
cd retia-wasm
CARGO_PROFILE_RELEASE_LTO=fat wasm-pack build --target web --release
```

The `--target web` option is required for the usage pattern below. See the [wasm-pack docs](https://rustwasm.github.io/wasm-pack/book/commands/build.html#target) for other targets.

## Usage

```js
import init, { RetiaDb } from "retia-wasm";

let db;
init().then(() => {
    db = RetiaDb.new();
    // db can only be used after the promise resolves
});
```

## API

```ts
export class RetiaDb {
    free(): void;
    static new(): RetiaDb;
    run(script: string, params: string): string;
    export_relations(data: string): string;
    // Triggers are NOT run for the relations. If you need triggers, use queries with parameters.
    import_relations(data: string): string;
}
```

Note that this API is synchronous. Long-running computations will block the main thread; consider a web worker for heavy queries (note: ECMAScript-module workers have [limited browser support](https://developer.mozilla.org/en-US/docs/Web/API/Worker/Worker#browser_compatibility)).

For the query language (CozoScript), see the upstream [CozoDB docs](https://docs.cozodb.org/en/latest/index.html) — the syntax is unchanged in this fork.
