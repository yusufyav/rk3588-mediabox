cmake_minimum_required(VERSION 3.20)
project(rk3588-mediabox LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Release)
endif()
add_compile_options(-Wall -Wextra)

find_package(PkgConfig REQUIRED)

# MEDIABOX_FFMPEG_PREFIX points at an RKMPP-enabled FFmpeg. On the current
# target that is the ScreenBridge media stack; this is a deliberate temporary
# dependency for Gate MP1a and is tracked in docs/architecture.md.
set(MEDIABOX_FFMPEG_PREFIX "/opt/rk3588-screenbridge" CACHE PATH
    "Prefix of an RKMPP-enabled FFmpeg installation")
set(ENV{PKG_CONFIG_PATH} "${MEDIABOX_FFMPEG_PREFIX}/lib/pkgconfig:$ENV{PKG_CONFIG_PATH}")

pkg_check_modules(LIBDRM REQUIRED IMPORTED_TARGET libdrm)
pkg_check_modules(FFMPEG REQUIRED IMPORTED_TARGET
                  libavformat libavcodec libavutil)

add_executable(hdr-signaling-probe tools/hdr-signaling-probe.cpp)
target_include_directories(hdr-signaling-probe PRIVATE
                           ${LIBDRM_INCLUDE_DIRS} ${FFMPEG_INCLUDE_DIRS})
target_link_libraries(hdr-signaling-probe PRIVATE
                      PkgConfig::FFMPEG PkgConfig::LIBDRM)

install(TARGETS hdr-signaling-probe RUNTIME DESTINATION bin)
