#ifndef CR_TEST_LINUX_HOOKS_H
#define CR_TEST_LINUX_HOOKS_H

#include <stddef.h>
#include <stdint.h>

void *cr_test_epoll_calloc(size_t count, size_t size);
void cr_test_epoll_free(void *pointer);
void cr_test_epoll_fd_opened(int fd);
void cr_test_epoll_fd_closed(int fd);
void cr_test_epoll_submit_observed(
    const void *operation,
    uint64_t generation,
    uint64_t token
);
void cr_test_epoll_before_recv(
    const void *operation,
    uint64_t generation,
    int fd
);
void cr_test_epoll_rearmed(
    const void *operation,
    uint64_t generation,
    uint64_t token
);
uint64_t cr_test_epoll_filter_event_token(uint64_t token);

#define CR_BACKEND_EPOLL_CALLOC cr_test_epoll_calloc
#define CR_BACKEND_EPOLL_FREE cr_test_epoll_free
#define CR_BACKEND_EPOLL_FD_OPENED(fd) cr_test_epoll_fd_opened((fd))
#define CR_BACKEND_EPOLL_FD_CLOSED(fd) cr_test_epoll_fd_closed((fd))
#define CR_BACKEND_EPOLL_SUBMIT_OBSERVED(operation, generation, token) \
    cr_test_epoll_submit_observed((operation), (generation), (token))
#define CR_BACKEND_EPOLL_BEFORE_RECV(operation, generation, fd) \
    cr_test_epoll_before_recv((operation), (generation), (fd))
#define CR_BACKEND_EPOLL_REARMED(operation, generation, token) \
    cr_test_epoll_rearmed((operation), (generation), (token))
#define CR_BACKEND_EPOLL_FILTER_EVENT_TOKEN(token) \
    cr_test_epoll_filter_event_token((token))

#endif
