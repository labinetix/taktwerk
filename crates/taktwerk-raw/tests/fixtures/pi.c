#include <stdlib.h>
#include "pi.h"

typedef struct { int n; double dt; double *integral; } pi_state;

int pi_create(void **handle, int n, double dt) {
    pi_state *s;
    int i;
    if (n < 1) return -2;
    s = (pi_state *)malloc(sizeof *s);
    if (!s) return -3;
    s->n = n;
    s->dt = dt;
    s->integral = (double *)malloc(sizeof(double) * (size_t)n);
    if (!s->integral) { free(s); return -3; }
    for (i = 0; i < n; i++) s->integral[i] = 0.0;
    *handle = s;
    return 0;
}

int pi_step(void *handle, const double *sp, const double *pv, double *out, const double *kp, double ki, int n) {
    pi_state *s = (pi_state *)handle;
    int i;
    if (!s || n != s->n) return -1;
    for (i = 0; i < n; i++) {
        double e = sp[i] - pv[i];
        s->integral[i] += ki * e * s->dt;
        out[i] = kp[i] * e + s->integral[i];
    }
    return 0;
}

int pi_destroy(void *handle) {
    pi_state *s = (pi_state *)handle;
    if (!s) return -1;
    free(s->integral);
    free(s);
    return 0;
}
