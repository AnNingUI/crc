#ifndef CR_TEST_WINDOWS_HOOKS_H
#define CR_TEST_WINDOWS_HOOKS_H

#include <stddef.h>

void *cr_test_iocp_calloc(size_t count, size_t size);
void cr_test_iocp_free(void *pointer);
void cr_test_iocp_handle_opened(void *handle);
void cr_test_iocp_handle_closed(void *handle);
void cr_test_iocp_submit_observed(
    const void *operation,
    int completed_inline
);

#define CR_BACKEND_IOCP_CALLOC cr_test_iocp_calloc
#define CR_BACKEND_IOCP_FREE cr_test_iocp_free
#define CR_BACKEND_IOCP_HANDLE_OPENED(handle) \
    cr_test_iocp_handle_opened((void *)(handle))
#define CR_BACKEND_IOCP_HANDLE_CLOSED(handle) \
    cr_test_iocp_handle_closed((void *)(handle))
#define CR_BACKEND_IOCP_SUBMIT_OBSERVED(operation, completed_inline) \
    cr_test_iocp_submit_observed((operation), (completed_inline))

#endif
