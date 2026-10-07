#include <stddef.h>
#include "ss.h"

static double x[SS_MAX];
static double xn[SS_MAX];
static int nx = 0;
static int inited = 0;
static double dt_seen = 0.0;

int ss_init(const ss_params *p, double dt) {
    int i;
    if (p->nx < 1 || p->nx > SS_MAX) return -2;
    nx = p->nx;
    for (i = 0; i < nx; i++) x[i] = 0.0;
    dt_seen = dt;
    inited = 1;
    return 0;
}

int ss_step(const ss_params *p, ss_io *io, double t) {
    int i, j;
    (void)t;
    if (!inited || p->nx != nx) return -1;
    for (i = 0; i < nx; i++) {
        double acc = 0.0;
        for (j = 0; j < nx; j++) acc += p->A[i * nx + j] * x[j];
        xn[i] = acc + p->b[i] * io->u;
    }
    io->y = 0.0;
    for (i = 0; i < nx; i++) {
        x[i] = xn[i];
        io->x[i] = x[i];
        io->y += p->c[i] * x[i];
    }
    io->y *= p->k;
    return 0;
}

void ss_terminate(void) { inited = 0; nx = 0; }

int ss_sizeof_params(void) { return (int)sizeof(ss_params); }
int ss_sizeof_io(void) { return (int)sizeof(ss_io); }
