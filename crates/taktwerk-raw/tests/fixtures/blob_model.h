/* A model behind a single entry point: one function for init and step, called with an id and
 * two opaque byte pointers that the library casts to its input and output structs. The library
 * keeps no state; every call re-reads the sizes it reported from the caller's output struct. */
#ifndef BLOB_MODEL_H
#define BLOB_MODEL_H

#include <stdint.h>

#define BLOB_ID 7

#define BLOB_OK 0
#define BLOB_NULL_ARGUMENT (-1)
#define BLOB_UNKNOWN_VARIANT (-2)
#define BLOB_UNKNOWN_ID (-3)
#define BLOB_NOT_INITIALISED (-4)
#define BLOB_BAD_FLAG (-5)

/* What a successful call writes into `ready`: a true byte that is not 1. */
#define BLOB_TRUE_BYTE 2

typedef struct {
    int32_t *init_flag; /* 1 on the first call (init), 0 on every step */
    int32_t variant;    /* by value between two pointers: which size set to report */
    double *k;          /* scalar gain */
    double *u;          /* nu inputs */
    int32_t *counter;   /* step counter, written back by the library */
    char *title;        /* text the caller supplies, NUL-terminated */
} blob_input;

typedef struct {
    char *label;      /* the title echoed back, or the library's own name if empty */
    double *y;        /* ny outputs: y = k * gain * u */
    double *gain;     /* ny x nu, row-major, filled at init */
    double *p;        /* np parameters, filled at init */
    char *names;      /* names written at init */
    int32_t *dim_nu;  /* reported at init */
    int32_t *dim_ny;  /* reported at init */
    int32_t *dim_np;  /* reported at init */
    uint8_t *ready;   /* BLOB_TRUE_BYTE after every successful call */
} blob_output;

int blob_call(int id, char *in, char *out);

#endif
