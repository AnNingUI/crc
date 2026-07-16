cr_awaitable external_value(int input);

__async int child_value(int input) {
    __yield input;
    return input + 1;
}

__async int representative(int input) {
    __async int bound = child_value(input);
    int first = __await bound;
    int second = __await child_value(first);
    int third = __await external_value(second);
    return third;
}
