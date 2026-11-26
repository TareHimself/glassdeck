cmake_minimum_required(VERSION 3.25 FATAL_ERROR)

project(glassdeck-luau-check-native LANGUAGES CXX VERSION 1.0.0)

set(CMAKE_CXX_STANDARD 17)

add_library(glassdeck_luau_check SHARED src/glassdeck_luau_check.cpp src/glassdeck_luau_check.h)
target_include_directories(glassdeck_luau_check PUBLIC src)

find_package(luau REQUIRED)
target_link_libraries(glassdeck_luau_check PRIVATE luau::analysis)
target_compile_definitions(glassdeck_luau_check PRIVATE GD_LUAU_PRODUCER)

install(TARGETS glassdeck_luau_check RUNTIME DESTINATION bin LIBRARY DESTINATION bin)
if(SILO_RUNTIME_LIBRARIES)
    install(FILES ${SILO_RUNTIME_LIBRARIES} DESTINATION bin)
endif()
