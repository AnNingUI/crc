#ifndef CR_TEST_BACKEND_DIFFERENTIAL_TRANSCRIPT_H
#define CR_TEST_BACKEND_DIFFERENTIAL_TRANSCRIPT_H

#include <stdint.h>

#if defined(CR_BACKEND_DIFFERENTIAL)
#include <stdio.h>
#include <stdlib.h>

static void cr_test_diff_emit(
    const char *scenario,
    uint32_t terminal,
    uint64_t bytes,
    uint32_t category,
    uint32_t callbacks,
    uint32_t wakes,
    uint32_t quiescent,
    uint32_t reusable,
    uint32_t pump_reason,
    uint32_t events
) {
    int written = printf(
        "CRDIFF %s terminal=%u bytes=%llu category=%u callbacks=%u "
        "wakes=%u quiescent=%u reusable=%u pump=%u events=%u\n",
        scenario,
        (unsigned int)terminal,
        (unsigned long long)bytes,
        (unsigned int)category,
        (unsigned int)callbacks,
        (unsigned int)wakes,
        (unsigned int)quiescent,
        (unsigned int)reusable,
        (unsigned int)pump_reason,
        (unsigned int)events
    );
    if (written < 0) {
        abort();
    }
}

#else

#define cr_test_diff_emit(...) ((void)0)

#endif

#endif
