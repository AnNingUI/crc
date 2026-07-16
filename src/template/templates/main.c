#include <stdio.h>

#include "main.h"

int main(void) {
    cr_error create_error = {0, NULL};
    cr_sequence_task *task = cr_sequence_create(&create_error);
    if (task == NULL) {
        fprintf(
            stderr,
            "failed to create sequence task: %s\n",
            create_error.message != NULL ? create_error.message : "unknown error"
        );
        return 1;
    }

    cr_poll_status status = cr_sequence_poll(task, NULL);
    if (status != CR_POLL_YIELDED) {
        cr_sequence_destroy(task);
        return 2;
    }
    printf("{{ project_name }} yielded %d\n", *cr_sequence_yielded(task));

    status = cr_sequence_poll(task, NULL);
    if (status != CR_POLL_READY) {
        cr_sequence_destroy(task);
        return 3;
    }
    printf("{{ project_name }} completed with %d\n", *cr_sequence_result(task));

    cr_sequence_destroy(task);
    return 0;
}
