/* Prints the layout gcc gives the blob structs: one line per struct, size then offsets. */
#include <stddef.h>
#include <stdio.h>
#include "blob_model.h"

int main(void) {
    printf("blob_input %zu %zu %zu %zu %zu %zu %zu\n", sizeof(blob_input),
           offsetof(blob_input, init_flag), offsetof(blob_input, variant), offsetof(blob_input, k),
           offsetof(blob_input, u), offsetof(blob_input, counter), offsetof(blob_input, title));
    printf("blob_output %zu %zu %zu %zu %zu %zu %zu %zu %zu %zu\n", sizeof(blob_output),
           offsetof(blob_output, label), offsetof(blob_output, y), offsetof(blob_output, gain),
           offsetof(blob_output, p), offsetof(blob_output, names), offsetof(blob_output, dim_nu),
           offsetof(blob_output, dim_ny), offsetof(blob_output, dim_np),
           offsetof(blob_output, ready));
    return 0;
}
