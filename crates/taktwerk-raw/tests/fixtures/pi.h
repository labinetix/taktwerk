/* A PI controller over n channels; each instance behind a handle. */
#ifndef PI_H
#define PI_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

int pi_create(void **handle, int n, double dt);
int pi_step(void *handle, const double *sp, const double *pv, double *out, const double *kp, double ki, int n);
int pi_destroy(void *handle);

#ifdef __cplusplus
}
#endif
#endif
