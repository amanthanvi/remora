cmake_minimum_required(VERSION 3.22.1)

project(remora_ghostty_jni LANGUAGES CXX)

add_library(ghostty SHARED IMPORTED)
set_target_properties(
    ghostty
    PROPERTIES
        IMPORTED_LOCATION "${CMAKE_CURRENT_SOURCE_DIR}/../jniLibs/${ANDROID_ABI}/libghostty.so"
        INTERFACE_INCLUDE_DIRECTORIES "${CMAKE_CURRENT_SOURCE_DIR}/include"
)

add_library(remora_ghostty_jni SHARED ghostty_jni.cpp)

target_compile_features(remora_ghostty_jni PRIVATE cxx_std_17)
target_include_directories(remora_ghostty_jni PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_link_libraries(remora_ghostty_jni PRIVATE ghostty EGL GLESv3 android log)
