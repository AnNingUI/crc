cmake_minimum_required(VERSION 3.16)
project({{ project_name }} LANGUAGES C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

set(CR_DIST_DIR "${CMAKE_SOURCE_DIR}/crc/dist")
file(GLOB_RECURSE CR_GENERATED_SOURCES CONFIGURE_DEPENDS
    "${CR_DIST_DIR}/*.c"
)
file(GLOB_RECURSE PROJECT_SOURCES CONFIGURE_DEPENDS
    "${CMAKE_SOURCE_DIR}/src/*.c"
)

if(NOT CR_GENERATED_SOURCES)
    message(FATAL_ERROR "Run 'crc build' before configuring this project")
endif()

add_executable(${PROJECT_NAME}
    ${PROJECT_SOURCES}
    ${CR_GENERATED_SOURCES}
)

include("${CR_DIST_DIR}/crc-generated-dependencies.cmake" OPTIONAL)
if(CR_GENERATED_DEPENDENCIES)
    target_link_libraries(${PROJECT_NAME} PRIVATE
        ${CR_GENERATED_DEPENDENCIES}
    )
endif()

target_include_directories(${PROJECT_NAME} PRIVATE
    "${CMAKE_SOURCE_DIR}/include"
    "${CR_DIST_DIR}/include"
)

if(MSVC)
    target_compile_options(${PROJECT_NAME} PRIVATE /W4)
else()
    target_compile_options(${PROJECT_NAME} PRIVATE -Wall -Wextra -Wpedantic)
endif()
