cmake_minimum_required(VERSION 3.22.1)

project(oniterminal LANGUAGES CXX)

add_library(ghostty-vt SHARED IMPORTED)
set_target_properties(
  ghostty-vt
  PROPERTIES IMPORTED_LOCATION
             "${CMAKE_CURRENT_SOURCE_DIR}/../jniLibs/${ANDROID_ABI}/libghostty-vt.so"
)

set(ONI_TERMINAL_CORE_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../../../../cpp")

add_library(oniterminal SHARED oni_terminal_jni.cpp "${ONI_TERMINAL_CORE_DIR}/oni_terminal_core.cpp")

target_include_directories(
  oniterminal
  PRIVATE "${ONI_TERMINAL_CORE_DIR}"
          "${CMAKE_CURRENT_SOURCE_DIR}/../../../../../../../../native/libghostty-vt/include"
)

target_link_options(
  oniterminal PRIVATE "-Wl,-z,common-page-size=16384" "-Wl,-z,max-page-size=16384"
)

target_link_libraries(oniterminal PRIVATE ghostty-vt log)
