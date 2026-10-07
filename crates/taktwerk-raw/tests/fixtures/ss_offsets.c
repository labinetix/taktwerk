/* Prints the layout gcc gives the fixture structs: one line per struct, size then offsets. */
#include <stddef.h>
#include <stdio.h>
#include "ss.h"

int main(void) {
    printf("ss_params %zu %zu %zu %zu %zu %zu\n", sizeof(ss_params), offsetof(ss_params, nx),
           offsetof(ss_params, A), offsetof(ss_params, b), offsetof(ss_params, c),
           offsetof(ss_params, k));
    printf("ss_io %zu %zu %zu %zu\n", sizeof(ss_io), offsetof(ss_io, u), offsetof(ss_io, x),
           offsetof(ss_io, y));
    return 0;
}
