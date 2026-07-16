__async int fetch(int socket) {
    int bytes = __await read_socket(socket);
    int alias_bytes = __awite read_socket(socket);
    __yield bytes + alias_bytes;
    __defer close_socket(socket);
    return bytes;
}

__async int task = fetch(1);
