/* Drives the wrapped blob FMU through the Modelica Association Reference-FMUs import library
 * (FMI.c, FMI3.c): an importer taktwerk did not write. Prints what it reads, one value per
 * line, for the test to compare with the raw adapter.
 *
 *   fmi_import_driver <platform binary> <instantiation token> <resources dir> */
#include <stdio.h>
#include <stdlib.h>

#include "FMI.h"
#include "FMI3.h"

static void log_message(FMIInstance *instance, FMIStatus status, const char *category,
                        const char *message) {
    (void)instance;
    (void)category;
    if (status > FMIOK) fprintf(stderr, "fmu: %s\n", message);
}

#define CALL(f)                                                        \
    do {                                                               \
        FMIStatus s = (f);                                             \
        if (s > FMIOK) {                                               \
            fprintf(stderr, "%s failed with status %d\n", #f, (int)s); \
            return EXIT_FAILURE;                                       \
        }                                                              \
    } while (0)

int main(int argc, char **argv) {
    if (argc != 4) return EXIT_FAILURE;
    FMIInstance *S = FMICreateInstance("blob", log_message, NULL);
    if (!S) return EXIT_FAILURE;
    CALL(FMILoadPlatformBinary(S, argv[1]));
    CALL(FMI3InstantiateCoSimulation(S, argv[2], argv[3], fmi3False, fmi3False, fmi3False,
                                     fmi3False, NULL, 0, NULL));

    /* Value references as modelDescription.xml lists them: time 0, nu 1, ny 2, np 3, then
     * k 4, u 5, counter 6, title 7, label 8, y 9, gain 10, p 11, names 12, ready 13. */
    const fmi3ValueReference dims[] = {1, 2, 3};
    const fmi3UInt64 sizes[] = {2, 3, 4};
    CALL(FMI3EnterConfigurationMode(S));
    CALL(FMI3SetUInt64(S, dims, 3, sizes, 3));
    CALL(FMI3ExitConfigurationMode(S));

    const fmi3ValueReference vr_k = 4, vr_u = 5, vr_counter = 6, vr_title = 7, vr_label = 8,
                             vr_y = 9, vr_gain = 10, vr_names = 12, vr_ready = 13;
    const fmi3Float64 k = 2.0;
    CALL(FMI3SetFloat64(S, &vr_k, 1, &k, 1));
    const fmi3String title = "tank 3";
    CALL(FMI3SetString(S, &vr_title, 1, &title, 1));
    CALL(FMI3EnterInitializationMode(S, fmi3False, 0.0, 0.0, fmi3False, 0.0));
    const fmi3Float64 u0[2] = {0.0, 0.0};
    CALL(FMI3SetFloat64(S, &vr_u, 1, u0, 2));
    CALL(FMI3ExitInitializationMode(S));

    fmi3String text = NULL;
    CALL(FMI3GetString(S, &vr_label, 1, &text, 1));
    printf("label %s\n", text);
    CALL(FMI3GetString(S, &vr_names, 1, &text, 1));
    printf("names %s\n", text);
    fmi3Float64 gain[6];
    CALL(FMI3GetFloat64(S, &vr_gain, 1, gain, 6));
    for (int i = 0; i < 6; i++) printf("gain %.17g\n", gain[i]);
    fmi3UInt8 ready = 0;
    CALL(FMI3GetUInt8(S, &vr_ready, 1, &ready, 1));
    printf("ready %u\n", (unsigned)ready);

    for (int step = 1; step <= 5; step++) {
        if (step == 4) {
            const fmi3Float64 k2 = -0.5;
            CALL(FMI3SetFloat64(S, &vr_k, 1, &k2, 1));
        }
        const fmi3Float64 u[2] = {0.5 * step, -1.0};
        CALL(FMI3SetFloat64(S, &vr_u, 1, u, 2));
        fmi3Boolean event = fmi3False, terminate = fmi3False, early = fmi3False;
        fmi3Float64 last = 0.0;
        CALL(FMI3DoStep(S, 0.1 * (step - 1), 0.1, fmi3True, &event, &terminate, &early, &last));
        fmi3Float64 y[3];
        CALL(FMI3GetFloat64(S, &vr_y, 1, y, 3));
        for (int i = 0; i < 3; i++) printf("y %.17g\n", y[i]);
        fmi3Int32 counter = 0;
        CALL(FMI3GetInt32(S, &vr_counter, 1, &counter, 1));
        printf("counter %d\n", (int)counter);
    }
    CALL(FMI3Terminate(S));
    FMI3FreeInstance(S);
    FMIFreeInstance(S);
    return EXIT_SUCCESS;
}
