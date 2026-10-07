/* A discrete state-space model, size-generic in nx, state in globals. */
#ifndef SS_H
#define SS_H

#define SS_MAX 64

typedef struct {
    int nx;
    const double *A; /* nx*nx, row-major */
    const double *b; /* nx */
    const double *c; /* nx */
    double k;        /* output gain */
} ss_params;

typedef struct {
    double u;
    double *x; /* nx, written by the library */
    double y;
} ss_io;

int ss_init(const ss_params *p, double dt);
int ss_step(const ss_params *p, ss_io *io, double t);
void ss_terminate(void);

/* layout oracle for tests */
int ss_sizeof_params(void);
int ss_sizeof_io(void);

#endif
