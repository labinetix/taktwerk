/* A PI controller over n channels, one state per instance behind a handle. */
#ifndef PI_H
#define PI_H

typedef struct {
    double kp;
    double ki;
} pi_gains;

int pi_init(void **handle, int n, double dt);
int pi_step(void *handle, const pi_gains *gains, const double *sp, const double *pv, double *u);
void pi_terminate(void *handle);

#endif
