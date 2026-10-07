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
typedef struct { int nx; int nu; /* one int per dimension */ } <m>_dims;
typedef struct { /* parameters */ } <m>_params;
typedef struct { /* inputs */ }     <m>_inputs;
typedef struct { /* tunables */ }   <m>_tunables;
typedef struct { /* outputs */ }    <m>_outputs;

int <m>_init(void **h, const <m>_dims *dims, const <m>_params *params, double step_size);
int <m>_step(void *h, double time, const <m>_inputs *in, const <m>_tunables *tun,
             <m>_outputs *out);
int <m>_terminate(void *h);
```

The rules:

- **Return codes.** Every function returns `int`; `0` is success, anything else an error. A
  failed init or step stops the engine (fail-stop); the code is logged.
- **Sizes in `<m>_dims`,** one `int` per dimension, named like the dimension.
- **Arrays are pointers** whose memory the engine owns. Every array member carries its shape in
  a trailing comment, in dimension names: `/* [n] */`, `/* [nx][nx] */`. Matrices are
  row-major. Pointers stay valid for the instance's lifetime, so init may keep them.
- **Scalars by value** inside the structs: a scalar input, tunable or output is a plain `double`
  (or another admitted type) member.
- **`const` marks what the model reads:** dims, params, inputs and tunables are `const`; outputs
  are not.
- **State behind the handle.** `init` allocates the instance's state and stores it through `h`;
  `step` and `terminate` receive it. No globals, so any number of instances share one loaded
  library (`instances = "multiple"`).
- **No allocation in step,** no blocking I/O, no printing. Allocate everything at init and free
  it in `terminate`.
- **Admitted C types:** `double`, `float`, `bool`, `char`, and the fixed-width and plain integer
  types at their 64-bit Linux widths, each optionally `const` and optionally followed by one `*`.

`step` advances from `time` by `step_size`. The engine fills `in` and `tun` before each step and
reads `out` after it; a changed tunable is in `tun` from the next step on.

## Example: a low-pass filter bank

A first-order low-pass filter per channel over `n` channels, `y = gain · x`, with
`x ← x + α (u − x)` and `α = dt / (tau + dt)`.

`lowpass.h`:

```c
/* A bank of first-order low-pass filters over n channels. */
#ifndef LOWPASS_H
#define LOWPASS_H

typedef struct {
    int n;
} lowpass_dims;

typedef struct {
    const double *tau;  /* [n] */
    const double *y0;   /* [n] */
} lowpass_params;

typedef struct {
    const double *u;    /* [n] */
} lowpass_inputs;

typedef struct {
    double gain;
} lowpass_tunables;

typedef struct {
    double *y;          /* [n] */
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
    double *y;     /* [n], filter state */
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
    s->y = calloc((size_t)s->n, sizeof *s->y);
    if (!s->alpha || !s->y) {
        lowpass_terminate(s);
        return 2;
    }
    for (i = 0; i < s->n; i++) {
        if (params->tau[i] < 0.0) {
            lowpass_terminate(s);
            return 3;
        }
        s->alpha[i] = step_size / (params->tau[i] + step_size);
        s->y[i] = params->y0[i];
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
        s->y[i] += s->alpha[i] * (in->u[i] - s->y[i]);
        out->y[i] = tun->gain * s->y[i];
    }
    return 0;
}

int lowpass_terminate(void *h) {
    lowpass_state *s = h;
    if (s) {
        free(s->alpha);
        free(s->y);
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

## Descriptor: import, then confirm

`taktwerk import-header` reads the header and proposes a descriptor:

```sh
taktwerk import-header lowpass.h -o taktwerk-model.toml
```

The proposal is a guess by name and is written with `abi.confirmed = false`, which the engine
refuses to load. Its leading comment lists every guess. The importer does not yet read the shape
comments or the struct roles of the recommended shape; **review the proposal and confirm it.**
For `lowpass.h` it proposes:

```text
# - step = lowpass_step (by name)
# - init = lowpass_init (by name)
# - lowpass_params: tau sized by n (guessed)
# - lowpass_params: y0 sized by n (guessed)
# - lowpass_init: step_size taken as StepSize
# - lowpass_step: time taken as Time
# - lowpass_inputs: u sized by n (guessed)
# - struct lowpass_tunables: gain by value is Parameter (guessed)
# - lowpass_outputs: y sized by n (guessed)
# - terminate = lowpass_terminate (by name)
# - a void * handle was found: instances = multiple
# - dimensions guessed from integer names: n; min = 1 assumed
# - matrices: set two shape entries and `layout` by hand
```

Two guesses need fixing: `tau` and `y0` are proposed as inputs (const pointers) and must become
`parameter`; `gain` is proposed as a parameter and must become `tunable`. Add a `default` for
`n` if the model has one, add units and descriptions, check every array's shape against its
comment, then set `confirmed = true`. The confirmed descriptor, written compactly:

```toml
name = "lowpass"
instances = "multiple"

[[dimensions]]
name = "n"
min = 1
default = 1

[[variables]]
name = "tau"
causality = "parameter"
type = "f64"
shape = ["n"]
unit = "s"

[[variables]]
name = "y0"
causality = "parameter"
type = "f64"
shape = ["n"]

[[variables]]
name = "u"
causality = "input"
type = "f64"
shape = ["n"]

[[variables]]
name = "gain"
causality = "tunable"
type = "f64"

[[variables]]
name = "y"
causality = "output"
type = "f64"
shape = ["n"]

[abi]
confirmed = true

[abi.init]
symbol = "lowpass_init"
args = [{ handle = "out" }, { struct = "lowpass_dims" }, { struct = "lowpass_params" }, { builtin = "step_size" }]

[abi.step]
symbol = "lowpass_step"
args = [{ handle = "in" }, { builtin = "time" }, { struct = "lowpass_inputs" }, { struct = "lowpass_tunables" }, { struct = "lowpass_outputs" }]

[abi.terminate]
symbol = "lowpass_terminate"
args = [{ handle = "in" }]

[abi.structs.lowpass_dims]
members = [{ name = "n", type = "int", dim = "n" }]

[abi.structs.lowpass_params]
members = [
  { name = "tau", type = "const double *", variable = "tau" },
  { name = "y0", type = "const double *", variable = "y0" },
]

[abi.structs.lowpass_inputs]
members = [{ name = "u", type = "const double *", variable = "u" }]

[abi.structs.lowpass_tunables]
members = [{ name = "gain", type = "double", variable = "gain" }]

[abi.structs.lowpass_outputs]
members = [{ name = "y", type = "double *", variable = "y" }]
```

`confirmed = true` states that you checked which pointer takes which dimension. Lengths are
never inferred at run time: a wrong shape here means the library reads or writes out of bounds.
The full format is in [the descriptor reference](descriptor.md).

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
