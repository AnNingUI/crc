#ifndef CR_TEST_MACOS_HOOKS_H
#define CR_TEST_MACOS_HOOKS_H

#include <stddef.h>
#include <stdint.h>
#include <sys/types.h>

void *cr_test_kqueue_calloc(size_t count, size_t size);
void cr_test_kqueue_free(void *pointer);
void cr_test_kqueue_fd_opened(int fd);
void cr_test_kqueue_fd_closed(int fd);
void cr_test_kqueue_submit_observed(
    const void *operation,
    uint64_t generation,
    uint64_t token
);
void cr_test_kqueue_before_recv(
    const void *operation,
    uint64_t generation,
    int fd
);
void cr_test_kqueue_rearmed(
    const void *operation,
    uint64_t generation,
    uint64_t token
);
uint64_t cr_test_kqueue_filter_event_token(uint64_t token);
void cr_test_kqueue_event_observed(
    uint64_t token,
    uint16_t flags,
    uint32_t fflags,
    intptr_t data
);
ssize_t cr_test_kqueue_recv(int fd, void *buffer, size_t size);

#define CR_BACKEND_KQUEUE_CALLOC cr_test_kqueue_calloc
#define CR_BACKEND_KQUEUE_FREE cr_test_kqueue_free
#define CR_BACKEND_KQUEUE_FD_OPENED(fd) cr_test_kqueue_fd_opened((fd))
#define CR_BACKEND_KQUEUE_FD_CLOSED(fd) cr_test_kqueue_fd_closed((fd))
#define CR_BACKEND_KQUEUE_SUBMIT_OBSERVED(operation, generation, token) \
    cr_test_kqueue_submit_observed((operation), (generation), (token))
#define CR_BACKEND_KQUEUE_BEFORE_RECV(operation, generation, fd) \
    cr_test_kqueue_before_recv((operation), (generation), (fd))
#define CR_BACKEND_KQUEUE_REARMED(operation, generation, token) \
    cr_test_kqueue_rearmed((operation), (generation), (token))
#define CR_BACKEND_KQUEUE_FILTER_EVENT_TOKEN(token) \
    cr_test_kqueue_filter_event_token((token))
#define CR_BACKEND_KQUEUE_EVENT_OBSERVED(token, flags, fflags, data) \
    cr_test_kqueue_event_observed((token), (flags), (fflags), (data))
#define CR_BACKEND_KQUEUE_RECV(fd, buffer, size) \
    cr_test_kqueue_recv((fd), (buffer), (size))

#endif
