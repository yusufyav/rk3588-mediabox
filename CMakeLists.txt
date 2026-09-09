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
# dependency for the MP1 gates and is tracked in docs/architecture.md.
set(MEDIABOX_FFMPEG_PREFIX "/opt/rk3588-screenbridge" CACHE PATH
    "Prefix of an RKMPP-enabled FFmpeg installation")
set(ENV{PKG_CONFIG_PATH} "${MEDIABOX_FFMPEG_PREFIX}/lib/pkgconfig:$ENV{PKG_CONFIG_PATH}")

pkg_check_modules(LIBDRM REQUIRED IMPORTED_TARGET libdrm)
pkg_check_modules(FFMPEG REQUIRED IMPORTED_TARGET
                  libavformat libavcodec libavutil)

# Gate MP1a's probe is deliberately left as a single self-contained file: its
# source hash is published in the MP1a report, and re-running that gate has to
# stay byte-for-byte reproducible.
add_executable(hdr-signaling-probe tools/hdr-signaling-probe.cpp)
target_include_directories(hdr-signaling-probe PRIVATE
                           ${LIBDRM_INCLUDE_DIRS} ${FFMPEG_INCLUDE_DIRS})
target_link_libraries(hdr-signaling-probe PRIVATE
                      PkgConfig::FFMPEG PkgConfig::LIBDRM)

# Gate MP1b onwards builds on shared modules instead. The DRM and decode
# structure is derived from the MP1a probe rather than shared with it, for the
# reason above.
add_library(mediabox-core STATIC
            src/common/log.cpp
            src/drm/display.cpp
            src/decode/decoder.cpp
            src/decode/framebuffer.cpp
            src/media/hdr_metadata.cpp
            src/media/cadence.cpp)
target_include_directories(mediabox-core PUBLIC
                           ${CMAKE_CURRENT_SOURCE_DIR}/src
                           ${LIBDRM_INCLUDE_DIRS} ${FFMPEG_INCLUDE_DIRS})
target_link_libraries(mediabox-core PUBLIC PkgConfig::FFMPEG PkgConfig::LIBDRM)

add_executable(hdr-playback-probe tools/hdr-playback-probe.cpp)
target_link_libraries(hdr-playback-probe PRIVATE mediabox-core)

install(TARGETS hdr-signaling-probe hdr-playback-probe RUNTIME DESTINATION bin)
