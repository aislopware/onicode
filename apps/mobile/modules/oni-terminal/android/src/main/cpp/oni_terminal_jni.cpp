#include <jni.h>

#include <cstdint>
#include <vector>

#include "oni_terminal_core.h"

namespace {

OniTerminal* FromHandle(jlong handle) {
  return reinterpret_cast<OniTerminal*>(static_cast<intptr_t>(handle));
}

jbyteArray ToJavaBytes(JNIEnv* env, OniTerminalBytes bytes) {
  auto result = env->NewByteArray(static_cast<jsize>(bytes.len));
  if (result != nullptr && bytes.len > 0) {
    env->SetByteArrayRegion(result, 0, static_cast<jsize>(bytes.len),
                            reinterpret_cast<const jbyte*>(bytes.data));
  }
  oni_terminal_bytes_free(bytes);
  return result;
}

// Keeps the Java palette copy alive for as long as the theme is used.
struct JavaTheme {
  JavaTheme(JNIEnv* env, jint foreground, jint background, jint cursor, jintArray palette_array) {
    if (palette_array != nullptr) {
      const auto length = env->GetArrayLength(palette_array);
      if (length > 0) {
        std::vector<jint> colors(static_cast<size_t>(length));
        env->GetIntArrayRegion(palette_array, 0, length, colors.data());
        palette.assign(colors.begin(), colors.end());
      }
    }
    theme = OniTerminalTheme{
        .foreground = static_cast<uint32_t>(foreground),
        .background = static_cast<uint32_t>(background),
        .cursor = static_cast<uint32_t>(cursor),
        .palette = palette.data(),
        .palette_len = palette.size(),
    };
  }

  std::vector<uint32_t> palette;
  OniTerminalTheme theme{};
};

}  // namespace

extern "C" JNIEXPORT jlong JNICALL
Java_expo_modules_oniterminal_GhosttyBridge_nativeCreate(
    JNIEnv* env, jclass, jint cols, jint rows, jint cell_width, jint cell_height,
    jint foreground, jint background, jint cursor, jintArray palette) {
  const JavaTheme theme(env, foreground, background, cursor, palette);
  return static_cast<jlong>(reinterpret_cast<intptr_t>(
      oni_terminal_create(cols, rows, cell_width, cell_height, &theme.theme)));
}

extern "C" JNIEXPORT void JNICALL
Java_expo_modules_oniterminal_GhosttyBridge_nativeDestroy(JNIEnv*, jclass, jlong handle) {
  oni_terminal_destroy(FromHandle(handle));
}

extern "C" JNIEXPORT jbyteArray JNICALL
Java_expo_modules_oniterminal_GhosttyBridge_nativeFeed(JNIEnv* env, jclass, jlong handle,
                                                       jbyteArray data) {
  if (FromHandle(handle) == nullptr || data == nullptr) return env->NewByteArray(0);
  const auto length = env->GetArrayLength(data);
  std::vector<uint8_t> bytes(static_cast<size_t>(length));
  if (length > 0) {
    env->GetByteArrayRegion(data, 0, length, reinterpret_cast<jbyte*>(bytes.data()));
  }
  return ToJavaBytes(env, oni_terminal_feed(FromHandle(handle), bytes.data(), bytes.size()));
}

extern "C" JNIEXPORT jbyteArray JNICALL
Java_expo_modules_oniterminal_GhosttyBridge_nativeResize(
    JNIEnv* env, jclass, jlong handle, jint cols, jint rows, jint cell_width, jint cell_height) {
  return ToJavaBytes(
      env, oni_terminal_resize(FromHandle(handle), cols, rows, cell_width, cell_height));
}

extern "C" JNIEXPORT void JNICALL
Java_expo_modules_oniterminal_GhosttyBridge_nativeScroll(JNIEnv*, jclass, jlong handle,
                                                         jint rows) {
  oni_terminal_scroll(FromHandle(handle), rows);
}

extern "C" JNIEXPORT jbyteArray JNICALL
Java_expo_modules_oniterminal_GhosttyBridge_nativeSetTheme(
    JNIEnv* env, jclass, jlong handle, jint foreground, jint background, jint cursor,
    jintArray palette) {
  const JavaTheme theme(env, foreground, background, cursor, palette);
  return ToJavaBytes(env, oni_terminal_set_theme(FromHandle(handle), &theme.theme));
}

extern "C" JNIEXPORT jboolean JNICALL
Java_expo_modules_oniterminal_GhosttyBridge_nativeSelectWordAt(JNIEnv*, jclass,
                                                               jlong handle, jint x,
                                                               jint y) {
  return oni_terminal_select_word_at(FromHandle(handle), x, y) ? JNI_TRUE : JNI_FALSE;
}

extern "C" JNIEXPORT void JNICALL
Java_expo_modules_oniterminal_GhosttyBridge_nativeExtendSelection(
    JNIEnv*, jclass, jlong handle, jint anchor_x, jint anchor_y, jint x, jint y) {
  oni_terminal_extend_selection(FromHandle(handle), anchor_x, anchor_y, x, y);
}

extern "C" JNIEXPORT jboolean JNICALL
Java_expo_modules_oniterminal_GhosttyBridge_nativeSelectAll(JNIEnv*, jclass,
                                                            jlong handle) {
  return oni_terminal_select_all(FromHandle(handle)) ? JNI_TRUE : JNI_FALSE;
}

extern "C" JNIEXPORT void JNICALL
Java_expo_modules_oniterminal_GhosttyBridge_nativeClearSelection(JNIEnv*, jclass,
                                                                 jlong handle) {
  oni_terminal_clear_selection(FromHandle(handle));
}

// Null when there is no selection.
extern "C" JNIEXPORT jbyteArray JNICALL
Java_expo_modules_oniterminal_GhosttyBridge_nativeGetSelectionText(JNIEnv* env, jclass,
                                                                   jlong handle) {
  OniTerminalBytes text{};
  if (!oni_terminal_selection_text(FromHandle(handle), &text)) return nullptr;
  return ToJavaBytes(env, text);
}

extern "C" JNIEXPORT jbyteArray JNICALL
Java_expo_modules_oniterminal_GhosttyBridge_nativeSnapshot(JNIEnv* env, jclass,
                                                           jlong handle) {
  return ToJavaBytes(env, oni_terminal_snapshot(FromHandle(handle)));
}
