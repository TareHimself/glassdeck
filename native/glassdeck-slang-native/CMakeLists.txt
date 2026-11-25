cmake_minimum_required(VERSION 3.25 FATAL_ERROR)

project(glassdeck-slang-native LANGUAGES CXX VERSION 1.0.0)

set(CMAKE_CXX_STANDARD 20)

add_library(glassdeck_slang SHARED src/glassdeck_slang.cpp src/glassdeck_slang.h)
target_include_directories(glassdeck_slang PUBLIC src)

find_package(slang REQUIRED)
target_link_libraries(glassdeck_slang PRIVATE slang::slang)
target_compile_definitions(glassdeck_slang PRIVATE GD_SLANG_PRODUCER)

install(TARGETS glassdeck_slang RUNTIME DESTINATION bin LIBRARY DESTINATION bin)
if(SILO_RUNTIME_LIBRARIES)
    install(FILES ${SILO_RUNTIME_LIBRARIES} DESTINATION bin)
endif()
