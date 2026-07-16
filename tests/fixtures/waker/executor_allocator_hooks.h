#ifndef TEST_EXECUTOR_ALLOCATOR_HOOKS_H
#define TEST_EXECUTOR_ALLOCATOR_HOOKS_H

#include <stddef.h>

void *test_executor_malloc(size_t size);
void *test_executor_calloc(size_t count, size_t size);
void test_executor_free(void *allocation);

#endif
