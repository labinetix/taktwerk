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
