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
