# Architecture Example: Image Codec Library + Application

How Stop types flow through a realistic codebase with an image codec
library (`imgcodec`) and an application that uses it.

## Crate dependency graph

```
app (binary)
├── almost-enough  → Stopper, StopToken
└── imgcodec (library)
    ├── enough         → Stop, StopReason, Unstoppable
    └── almost-enough  → StopToken (for internal type erasure + clone)
```

## Library: `imgcodec`

### Public API

`impl Stop + 'static` at the boundary. Erase to `StopToken` immediately.

```rust
use almost_enough::StopToken;
use enough::Stop;

pub fn decode(data: &[u8], stop: impl Stop + 'static) -> Result<Image, CodecError> {
    let stop = StopToken::new(stop); // erase once — Unstoppable becomes None
    let header = parse_header(data, &stop)?;
    decode_rows(data, &header, &stop)
}
```

Callers pass `Unstoppable` or `Stopper` — never hidden behind a
convenience wrapper.

### Internal: parallel fan-out

`StopToken` is `Clone` (Arc increment). Fan out to rayon tasks:

```rust
fn decode_rows(data: &[u8], header: &Header, stop: &StopToken) -> Result<Image, CodecError> {
    let rows: Vec<_> = (0..header.height)
        .into_par_iter()
        .map(|y| {
            let stop = stop.clone(); // Arc increment (free for Unstoppable — it's None)
            decode_single_row(data, header, y, &stop)
        })
        .collect();
    // ...
}
```

### Internal: hot loop

`StopToken.check()` handles the `Unstoppable` optimization automatically.
No `may_stop()` call needed — `None` path short-circuits internally:

```rust
fn decode_single_row(data: &[u8], header: &Header, y: usize, stop: &StopToken) -> Result<Row, CodecError> {
    let mut row = Row::new(header.width);
    for x in 0..header.width {
        if x % 256 == 0 {
            stop.check()?; // Unstoppable: no-op. Stopper: one dispatch.
        }
        row.pixels[x] = decode_pixel(data, header, x, y)?;
    }
    Ok(row)
}
```

### Without `almost-enough` (enough-only library)

If you don't want the `almost-enough` dep, use `&dyn Stop` with
`may_stop().then_some()`:

```rust
fn inner(data: &[u8], stop: &dyn Stop) -> Result<(), CodecError> {
    let stop = stop.may_stop().then_some(stop); // Option<&dyn Stop>
    for (i, chunk) in data.chunks(1024).enumerate() {
        if i % 64 == 0 {
            stop.check()?; // None → Ok(()), Some → one dispatch
        }
    }
    Ok(())
}
```

`Option<&dyn Stop>` implements `Stop` (from `enough`).

---

## Application

### No cancellation

```rust
use enough::Unstoppable;

let image = imgcodec::decode(&data, Unstoppable)?;
```

Explicit: "I chose no cancellation." Zero-cost internally — StopToken
stores `None`, all checks short-circuit.

### With cancellation

```rust
use almost_enough::Stopper;

let stopper = Stopper::new();
let cancel = stopper.clone();

std::thread::spawn(move || {
    std::thread::sleep(std::time::Duration::from_secs(5));
    cancel.cancel();
});

let image = imgcodec::decode(&data, stopper)?;
```

`Stopper` → `StopToken` conversion inside the library is zero-cost
(reuses the same Arc). All clones share one `AtomicBool`.

### Type-erased from external framework

```rust
use almost_enough::StopToken;

fn on_work_item(images: &[&[u8]], framework_token: FrameworkCancel) {
    let stop = StopToken::new(framework_token); // no Clone needed on T
    for data in images {
        imgcodec::decode(data, stop.clone())?; // Arc increment
    }
}
```

---

## What flows where

```
Application                       Library (imgcodec)
───────────                       ──────────────────

Stopper::new()
  │
  ├─→ decode(stop: impl Stop + 'static)
  │     │
  │     ├── StopToken::new(stop)        ← erase once (Stopper→StopToken: same Arc)
  │     │
  │     ├── decode_rows(&StopToken)
  │     │     ├── stop.clone() ──→ rayon task  (Arc increment)
  │     │     ├── stop.clone() ──→ rayon task
  │     │     └── ...
  │     │           └── decode_single_row(&StopToken)
  │     │                 └── stop.check()?   ← None: no-op. Some: one dispatch.
  │     │
  │     └── parse_header(&StopToken)
  │           └── stop.check()?
  │
  └── cancel.cancel() ─── Relaxed store ──→ same AtomicBool ──→ visible everywhere
```

## Cost summary

| Layer | Type | Unstoppable | Stopper |
|-------|------|-------------|---------|
| Public API | `impl Stop + 'static` | zero (erased to None) | one Arc clone |
| Fan-out | `StopToken::clone()` | free (None clone) | Arc increment |
| Hot loop | `StopToken::check()` | no-op (None match) | one vtable dispatch |
