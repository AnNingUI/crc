#ifndef CR_TEST_MACOS_HELPERS_H
#define CR_TEST_MACOS_HELPERS_H

#include <arpa/inet.h>
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <poll.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

typedef struct cr_test_socket_pair {
    int sender;
    int receiver;
} cr_test_socket_pair;

static void cr_test_set_close_on_exec(int fd) {
    int flags = fcntl(fd, F_GETFD, 0);
    assert(flags >= 0);
    assert(fcntl(fd, F_SETFD, flags | FD_CLOEXEC) == 0);
}

static cr_test_socket_pair cr_test_make_socket_pair(void) {
    int listener;
    int sender;
    int receiver;
    int flags;
    struct sockaddr_in address;
    socklen_t address_size = (socklen_t)sizeof(address);
    cr_test_socket_pair pair;

    listener = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    assert(listener >= 0);
    sender = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    assert(sender >= 0);
    cr_test_set_close_on_exec(listener);
    cr_test_set_close_on_exec(sender);
    memset(&address, 0, sizeof(address));
    address.sin_family = AF_INET;
    address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    address.sin_port = 0;
    assert(bind(
        listener,
        (const struct sockaddr *)&address,
        sizeof(address)
    ) == 0);
    assert(listen(listener, 1) == 0);
    assert(getsockname(
        listener,
        (struct sockaddr *)&address,
        &address_size
    ) == 0);
    assert(connect(
        sender,
        (const struct sockaddr *)&address,
        sizeof(address)
    ) == 0);
    receiver = accept(listener, NULL, NULL);
    assert(receiver >= 0);
    cr_test_set_close_on_exec(receiver);
    flags = fcntl(receiver, F_GETFL, 0);
    assert(flags >= 0);
    assert(fcntl(receiver, F_SETFL, flags | O_NONBLOCK) == 0);
    assert(close(listener) == 0);

    pair.sender = sender;
    pair.receiver = receiver;
    return pair;
}

static void cr_test_close_socket_pair(cr_test_socket_pair *pair) {
    if (pair->sender >= 0) {
        assert(close(pair->sender) == 0);
        pair->sender = -1;
    }
    if (pair->receiver >= 0) {
        assert(close(pair->receiver) == 0);
        pair->receiver = -1;
    }
}

static void cr_test_send_exact(int fd, const void *data, size_t data_size) {
    const unsigned char *bytes = (const unsigned char *)data;
    size_t sent = 0;

    while (sent < data_size) {
        ssize_t result = send(fd, bytes + sent, data_size - sent, 0);
        assert(result > 0);
        sent += (size_t)result;
    }
}

static void cr_test_wait_readable(int fd) {
    struct pollfd descriptor = {fd, POLLIN, 0};
    int result;

    do {
        result = poll(&descriptor, 1, 5000);
    } while (result < 0 && errno == EINTR);
    assert(result == 1);
    assert((descriptor.revents & (POLLIN | POLLHUP | POLLERR)) != 0);
}

#endif
