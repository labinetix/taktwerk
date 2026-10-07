#include "blob_model.h"

#include <stddef.h>
#include <string.h>

static int sizes(int32_t variant, int32_t *nu, int32_t *ny, int32_t *np) {
    switch (variant) {
    case 1: *nu = 2; *ny = 3; *np = 4; return 0;
    case 2: *nu = 1; *ny = 1; *np = 2; return 0;
    default: return -1;
    }
}

static void copy_text(char *dst, size_t cap, const char *src) {
    size_t n = strlen(src);
    if (n >= cap) n = cap - 1;
    memcpy(dst, src, n);
    dst[n] = '\0';
}

int blob_call(int id, char *in_bytes, char *out_bytes) {
    if (id != BLOB_ID) return BLOB_UNKNOWN_ID;
    if (in_bytes == NULL || out_bytes == NULL) return BLOB_NULL_ARGUMENT;
    blob_input *in = (blob_input *)in_bytes;
    blob_output *out = (blob_output *)out_bytes;
    if (in->init_flag == NULL || in->k == NULL || in->u == NULL || in->counter == NULL ||
        out->y == NULL || out->gain == NULL || out->p == NULL || out->dim_nu == NULL ||
        out->dim_ny == NULL || out->dim_np == NULL || out->ready == NULL)
        return BLOB_NULL_ARGUMENT;
    int32_t nu, ny, np;
    if (sizes(in->variant, &nu, &ny, &np) != 0) return BLOB_UNKNOWN_VARIANT;

    if (*in->init_flag == 1) {
        *out->dim_nu = nu;
        *out->dim_ny = ny;
        *out->dim_np = np;
        for (int i = 0; i < ny; i++) {
            out->y[i] = 0.0;
            for (int j = 0; j < nu; j++) out->gain[i * nu + j] = (i + 1) + 0.1 * j;
        }
        for (int i = 0; i < np; i++) out->p[i] = 0.5 * i;
        if (out->label != NULL)
            copy_text(out->label, 32, (in->title != NULL && in->title[0] != '\0') ? in->title
                                                                                   : "blob");
        if (out->names != NULL) copy_text(out->names, 24, "k;u;y;p");
        *in->counter = 0;
        *out->ready = BLOB_TRUE_BYTE;
        return BLOB_OK;
    }
    if (*in->init_flag != 0) return BLOB_BAD_FLAG;
    if (*out->dim_nu != nu || *out->dim_ny != ny || *out->dim_np != np) return BLOB_NOT_INITIALISED;
    for (int i = 0; i < ny; i++) {
        double acc = 0.0;
        for (int j = 0; j < nu; j++) acc += out->gain[i * nu + j] * in->u[j];
        out->y[i] = *in->k * acc;
    }
    *in->counter += 1;
    *out->ready = BLOB_TRUE_BYTE;
    return BLOB_OK;
}
