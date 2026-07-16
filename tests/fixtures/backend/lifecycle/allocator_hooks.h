#ifndef CR_BACKEND_LIFECYCLE_ALLOCATOR_HOOKS_H
#define CR_BACKEND_LIFECYCLE_ALLOCATOR_HOOKS_H

#include <stddef.h>

void *test_backend_calloc(size_t count, size_t size);
void test_backend_free(void *allocation);
void *test_provider_calloc(size_t count, size_t size);
void test_provider_free(void *allocation);
void *test_tracking_calloc(size_t count, size_t size);
void test_tracking_free(void *allocation);
void *test_awaitable_calloc(size_t count, size_t size);
void test_awaitable_free(void *allocation);
void *test_operation_calloc(size_t count, size_t size);
void test_operation_free(void *allocation);

#endif
