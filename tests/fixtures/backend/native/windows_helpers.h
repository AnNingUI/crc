#ifndef CR_TEST_WINDOWS_HELPERS_H
#define CR_TEST_WINDOWS_HELPERS_H

#ifndef _WIN32_WINNT
#define _WIN32_WINNT 0x0600
#endif

#include <winsock2.h>
#include <windows.h>

#include <assert.h>
#include <stdint.h>
#include <string.h>

typedef struct cr_test_socket_pair {
    SOCKET sender;
    SOCKET receiver;
} cr_test_socket_pair;

static cr_test_socket_pair cr_test_make_socket_pair(void) {
    SOCKET listener;
    SOCKET sender;
    SOCKET receiver;
    struct sockaddr_in address;
    int address_size = (int)sizeof(address);
    cr_test_socket_pair pair;

    listener = WSASocketW(
        AF_INET,
        SOCK_STREAM,
        IPPROTO_TCP,
        NULL,
        0,
        WSA_FLAG_OVERLAPPED
    );
    assert(listener != INVALID_SOCKET);
    memset(&address, 0, sizeof(address));
    address.sin_family = AF_INET;
    address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    address.sin_port = 0;
    assert(bind(
        listener,
        (const struct sockaddr *)&address,
        (int)sizeof(address)
    ) == 0);
    assert(listen(listener, 1) == 0);
    assert(getsockname(
        listener,
        (struct sockaddr *)&address,
        &address_size
    ) == 0);

    sender = WSASocketW(
        AF_INET,
        SOCK_STREAM,
        IPPROTO_TCP,
        NULL,
        0,
        WSA_FLAG_OVERLAPPED
    );
    assert(sender != INVALID_SOCKET);
    assert(connect(
        sender,
        (const struct sockaddr *)&address,
        (int)sizeof(address)
    ) == 0);
    receiver = accept(listener, NULL, NULL);
    assert(receiver != INVALID_SOCKET);
    assert(closesocket(listener) == 0);

    pair.sender = sender;
    pair.receiver = receiver;
    return pair;
}

static void cr_test_close_socket_pair(cr_test_socket_pair *pair) {
    if (pair->sender != INVALID_SOCKET) {
        assert(closesocket(pair->sender) == 0);
        pair->sender = INVALID_SOCKET;
    }
    if (pair->receiver != INVALID_SOCKET) {
        assert(closesocket(pair->receiver) == 0);
        pair->receiver = INVALID_SOCKET;
    }
}

static void cr_test_send_exact(
    SOCKET socket,
    const void *data,
    int data_size
) {
    const char *bytes = (const char *)data;
    int sent = 0;

    while (sent < data_size) {
        int result = send(socket, bytes + sent, data_size - sent, 0);
        assert(result > 0);
        sent += result;
    }
}

#endif
