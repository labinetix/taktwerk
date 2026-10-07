#include <stdlib.h>
#include "pi.h"

typedef struct {
    int n;
    double dt;
    double integral[];
} pi_state;

int pi_init(void **handle, int n, double dt) {
    pi_state *s;
    int i;
    if (n < 1 || dt <= 0.0) return -1;
    s = malloc(sizeof *s + sizeof(double) * (size_t)n);
    if (!s) return -2;
    s->n = n;
    s->dt = dt;
    for (i = 0; i < n; i++) s->integral[i] = 0.0;
    *handle = s;
    return 0;
}

int pi_step(void *handle, const pi_gains *gains, const double *sp, const double *pv, double *u) {
    pi_state *s = handle;
    int i;
    if (!s) return -1;
    for (i = 0; i < s->n; i++) {
        double e = sp[i] - pv[i];
        s->integral[i] += gains->ki * e * s->dt;
        u[i] = gains->kp * e + s->integral[i];
    }
    return 0;
}

void pi_terminate(void *handle) {
    free(handle);
}
