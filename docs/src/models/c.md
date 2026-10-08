# Hand-written C

A C model is a shared library plus a descriptor, `taktwerk-model.toml`, in a package directory:

```text
lowpass/
  taktwerk-model.toml      the descriptor
  lib/aarch64/liblowpass.so
  lib/x86_64/liblowpass.so
```

`lib/<arch>/` is named by the running architecture (`uname -m`). It holds one `.so`, or set
`abi.library` in the descriptor to pick one by file name.

## The recommended shape

New C models use this shape, for a model `<m>`:

```c
typedef struct { int nx; /* 1..64 = 4 */ int nu; } <m>_dims;   /* one int per dimension */
typedef struct { const double *A; /* [nx][nx] */ double k; } <m>_params;
typedef struct { const double *u; /* [nu] m/s: inflow */ } <m>_inputs;
typedef struct { double gain; } <m>_tunables;
typedef struct { double *x; /* [nx] */ double y; } <m>_outputs;

int <m>_init(void **h, const <m>_dims *dims, const <m>_params *params, double step_size);
int <m>_step(void *h, double time, const <m>_inputs *in, const <m>_tunables *tun,
             <m>_outputs *out);
int <m>_terminate(void *h);   /* or void */
```

`taktwerk import-header` reads a header in this shape completely and writes a descriptor that is
already confirmed: nothing is guessed, so there is nothing to review. The rules:

- **Return codes.** `init` and `step` return `int`; `0` is success, anything else an error. A
  failed init or step stops the engine (fail-stop); the code is logged. `terminate` returns
  `int` or `void`.
- **Roles by struct name.** The suffix of the type name says what a struct holds: `_dims`,
  `_params`, `_inputs`, `_tunables`, `_outputs`. Every member is a variable of its struct's
  role. `<m>_params` and `<m>_tunables` may be left out, and with them their argument. The
  struct arguments may come in any order; the handle, `time` and `step_size` are where the
  shape puts them.
- **Sizes in `<m>_dims`,** one `int` per dimension, named like the dimension. A trailing
  comment may bound it and give a default: `/* 1..64 */`, `/* 1.. */`, `/* 1..64 = 4 */`.
- **Arrays are pointers** whose memory the engine owns. Every pointer member carries its shape
  in a trailing comment on its line, in dimension names or literal lengths: `/* [n] */`,
  `/* [nx][nu] */`, `/* [3] */`. Matrices are row-major. Pointers stay valid for the
  instance's lifetime, so init may keep them.
- **Scalars by value** inside the structs: a scalar parameter, input, tunable or output is a
  plain `double` (or another admitted type) member without a shape.
- **Units and descriptions** may follow the shape in the same comment: `/* [n] s: time
  constant */` is unit `s` and description `time constant`; `/* [n]: input */` only a
  description. Without a colon, one word is a unit (`/* m */`) and more words a description.
- **`const` marks what the model reads:** dims, params, inputs and tunables are `const`; outputs
  are not.
- **State behind the handle.** `init` allocates the instance's state and stores it through `h`;
  `step` and `terminate` receive it. No globals, so any number of instances share one loaded
  library (`instances = "multiple"`).
- **No allocation in step,** no blocking I/O, no printing. Allocate everything at init and free
  it in `terminate`.
- **Admitted C types:** `double`, `float`, `bool`, `char`, and the fixed-width and plain integer
  types at their 64-bit Linux widths, each optionally `const` and optionally followed by one `*`.
  A `char *` with a literal length (`/* [32] */`) carries text, NUL included.

`step` advances from `time` by `step_size`. The engine fills `in` and `tun` before each step and
reads `out` after it; a changed tunable is in `tun` from the next step on.

## Example: a low-pass filter bank

A first-order low-pass filter per channel over `n` channels, `y = gain · x`, with
`x ← x + α (u − x)` and `α = dt / (tau + dt)`.

This is
[`examples/raw-filter-bank`](https://github.com/labinetix/taktwerk/tree/main/examples/raw-filter-bank).
`lowpass.h`:

```c
/* A bank of first-order low-pass filters over n channels, in the recommended shape. */
#ifndef LOWPASS_H
#define LOWPASS_H

typedef struct {
    int n;              /* 1..64 = 3 */
} lowpass_dims;

typedef struct {
    const double *tau;  /* [n] s: time constant */
    const double *y0;   /* [n]: initial state */
} lowpass_params;

typedef struct {
    const double *u;    /* [n]: filter input */
} lowpass_inputs;

typedef struct {
    double gain;        /* output gain */
} lowpass_tunables;

typedef struct {
    double *y;          /* [n]: gain times the filter state */
} lowpass_outputs;

int lowpass_init(void **h, const lowpass_dims *dims, const lowpass_params *params,
                 double step_size);
int lowpass_step(void *h, double time, const lowpass_inputs *in,
                 const lowpass_tunables *tun, lowpass_outputs *out);
int lowpass_terminate(void *h);

#endif
```

`lowpass.c`:

```c
#include <stdlib.h>
#include "lowpass.h"

typedef struct {
    int n;
    double *alpha; /* [n], dt / (tau + dt) */
    double *x;     /* [n], filter state */
} lowpass_state;

int lowpass_init(void **h, const lowpass_dims *dims, const lowpass_params *params,
                 double step_size) {
    lowpass_state *s;
    int i;
    if (dims->n < 1 || step_size <= 0.0) return 1;
    s = calloc(1, sizeof *s);
    if (!s) return 2;
    s->n = dims->n;
    s->alpha = calloc((size_t)s->n, sizeof *s->alpha);
    s->x = calloc((size_t)s->n, sizeof *s->x);
    if (!s->alpha || !s->x) {
        lowpass_terminate(s);
        return 2;
    }
    for (i = 0; i < s->n; i++) {
        if (params->tau[i] < 0.0) {
            lowpass_terminate(s);
            return 3;
        }
        s->alpha[i] = step_size / (params->tau[i] + step_size);
        s->x[i] = params->y0[i];
    }
    *h = s;
    return 0;
}

int lowpass_step(void *h, double time, const lowpass_inputs *in,
                 const lowpass_tunables *tun, lowpass_outputs *out) {
    lowpass_state *s = h;
    int i;
    (void)time;
    for (i = 0; i < s->n; i++) {
        s->x[i] += s->alpha[i] * (in->u[i] - s->x[i]);
        out->y[i] = tun->gain * s->x[i];
    }
    return 0;
}

int lowpass_terminate(void *h) {
    lowpass_state *s = h;
    if (s) {
        free(s->alpha);
        free(s->x);
        free(s);
    }
    return 0;
}
```

Build it into the package for the running architecture:

```sh
mkdir -p "lib/$(uname -m)"
cc -shared -fPIC -O2 -Wall -Wextra -o "lib/$(uname -m)/liblowpass.so" lowpass.c
```

## Descriptor: read from the header

`taktwerk import-header` reads the header and writes the descriptor:

```sh
taktwerk import-header lowpass.h --shape -o taktwerk-model.toml
```

The header is in the recommended shape, so the descriptor comes out with `abi.confirmed = true`
and the engine loads it as it is. `--shape` makes any deviation an error; without it, a header
that is not quite in the shape still gets a proposal (see below). For `lowpass.h`:

```toml
name = "lowpass"
instances = "multiple"

[[dimensions]]
name = "n"
min = 1
max = 64
default = 3

[[variables]]
name = "tau"
causality = "parameter"
type = "f64"
shape = ["n"]
layout = "row-major"
unit = "s"
description = "time constant"

[[variables]]
name = "y0"
causality = "parameter"
type = "f64"
shape = ["n"]
layout = "row-major"
description = "initial state"

[[variables]]
name = "u"
causality = "input"
type = "f64"
shape = ["n"]
layout = "row-major"
description = "filter input"

[[variables]]
name = "gain"
causality = "tunable"
type = "f64"
shape = []
layout = "row-major"
description = "output gain"

[[variables]]
name = "y"
causality = "output"
type = "f64"
shape = ["n"]
layout = "row-major"
description = "gain times the filter state"

[abi]
confirmed = true
# … init, step and terminate calls, one struct per role
```

The full format is in [the descriptor reference](descriptor.md).

## When the header deviates

A header that is not completely in the shape gets the heuristic proposal of
[other C interfaces](descriptor.md#workflow), written with `abi.confirmed = false`, which the
engine refuses to load. Its leading comment lists every deviation from the shape first, then
every guess. Leave out the shape comment of `tau`, for example:

```text
# - not the recommended shape: lowpass_params: tau is a pointer without a shape comment such as `/* [n] */`
```

Either fix the header and import again, or review the proposal, fix causalities and shapes by
hand and set `confirmed = true`. That flag states that you checked which pointer takes which
dimension: lengths are never inferred at run time, and a wrong shape means the library reads or
writes out of bounds.

## Run it

```toml
[engine]
tick_ms = 10.0

[models.lowpass]
kind = "raw"
path = "lowpass"

[[instance]]
id = "filt"
model = "lowpass"
dims = { n = 3 }
parameters = { tau = [0.1, 0.5, 1.0], y0 = [0.0, 0.0, 0.0], gain = 1.0 }

[[connector]]
id = "ua"
kind = "opcua-server"
endpoint = "opc.tcp://127.0.0.1:4840"
```

```sh
taktwerk inspect lowpass --kind raw
taktwerk check project.toml
taktwerk run project.toml
taktwerk tui opc.tcp://127.0.0.1:4840      # edit filt.u and filt.gain, watch filt.y
```

A second instance with another `n` is one more `[[instance]]` block on the same model.
