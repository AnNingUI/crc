#include "main.hr"

__async int sequence(void) {
    __yield 5;
    return 9;
}
